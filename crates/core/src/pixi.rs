//! The Pixi adapter: resolves `pixi.toml` and Pixi-managed `pyproject.toml`
//! targets with Pixi inside the stage, preserves Git pins, reports constraint
//! suggestions with availability evidence, and rewrites accepted constraints.
use crate::{
    adapter::{
        pypi_key, Adapter, AdapterSpec, Candidate, Capabilities, ManagedFiles, Support, ToolSpec,
    },
    constraints::{AvailabilityConfig, Declaration, Edit, RegistryConfig},
    process::run,
    Error, Result, Target, UpdateOptions,
};
use std::{collections::BTreeMap, fs, path::Path};

/// The Pixi adapter. Targets are `pixi.toml` files and `pyproject.toml` files
/// with a `[tool.pixi]` table; Pixi itself resolves in the stage.
pub struct Pixi;
impl Adapter for Pixi {
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            manager: "pixi".into(),
            ecosystems: vec!["conda".into(), "pypi".into()],
            patterns: vec!["pixi.toml".into(), "pyproject.toml".into()],
            // Native configuration and the lock are resolver inputs even when
            // .pixi or the lock is gitignored.
            managed: vec![ManagedFiles {
                manifests: vec!["pixi.toml".into(), "pyproject.toml".into()],
                inputs: vec![".pixi/config.toml".into(), "pixi.lock".into()],
            }],
            skip_dirs: vec![".pixi".into()],
            tools: vec![ToolSpec {
                name: "pixi".into(),
                default: "pixi".into(),
                tested_versions: vec!["0.80.0".into()],

                downloads: crate::provision::pinned("pixi"),
            }],
            capabilities: Capabilities {
                package_selection: Support::Supported,
                constraint_changes: Support::Supported,
                suggestion_acceptance: Support::Supported,
                git_refresh: Support::Supported,
                cooldown: Support::Unsupported(
                    "a common cooldown is not implemented; configure workspace.exclude-newer in the Pixi manifest"
                        .into(),
                ),
                install_validation: Support::Supported,
                lockfile: Support::Supported,
                platforms: Support::Supported,
            },
        }
    }
    fn detects(&self, path: &Path, content: &str) -> bool {
        path.file_name().is_some_and(|n| n == "pixi.toml")
            || (path.file_name().is_some_and(|n| n == "pyproject.toml")
                && content
                    .parse::<toml::Value>()
                    .ok()
                    .and_then(|v| v.get("tool")?.get("pixi").cloned())
                    .is_some())
    }
    fn inventory(&self, root: &Path, target: &Target) -> Result<Vec<crate::Package>> {
        let relative = target.manifest.parent().unwrap().join("pixi.lock");
        let text = match fs::read_to_string(root.join(&relative)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::Invalid(format!(
                    "{}: no {} to scan; create it with `pixi lock` or `depsmith update`",
                    target.id,
                    relative.display()
                )))
            }
            result => result?,
        };
        crate::inventory::pixi_inventory(&text)
    }
    fn select(
        &self,
        root: &Path,
        target: &Target,
        requested: &[String],
    ) -> Result<BTreeMap<String, String>> {
        let declared = self.declarations(root, target)?;
        let mut selected = BTreeMap::new();
        for request in requested {
            let found = declared.iter().find(|d| {
                if d.ecosystem == "pypi" {
                    pypi_key(&d.package) == pypi_key(request)
                } else {
                    d.package.eq_ignore_ascii_case(request.trim())
                }
            });
            if let Some(declaration) = found {
                selected.insert(request.clone(), declaration.package.clone());
            }
        }
        Ok(selected)
    }
    fn declarations(&self, root: &Path, target: &Target) -> Result<Vec<Declaration>> {
        let text = fs::read_to_string(root.join(&target.manifest))?;
        manifest_declarations(&text, is_pyproject(target), &target.manifest)
    }
    fn rewrite(&self, stage: &Path, target: &Target, edits: &[Edit]) -> Result<()> {
        if edits.is_empty() {
            return Ok(());
        }
        if let Some(edit) = edits.iter().find(|e| e.declaration.file != target.manifest) {
            return Err(Error::Invalid(format!(
                "{}: {} is not this target's manifest",
                target.id,
                edit.declaration.file.display()
            )));
        }
        let path = stage.join(&target.manifest);
        let text = rewrite_manifest(&fs::read_to_string(&path)?, is_pyproject(target), edits)?;
        fs::write(path, text)?;
        Ok(())
    }
    /// Conda packages are looked up with `pixi search` against the manifest's
    /// channels, PyPI packages on its indexes, both under its `exclude-newer`.
    fn availability(&self, root: &Path, target: &Target) -> Result<AvailabilityConfig> {
        let parsed: toml::Value = fs::read_to_string(root.join(&target.manifest))?
            .parse()
            .map_err(|e| Error::Invalid(format!("invalid manifest: {e}")))?;
        let pixi = if is_pyproject(target) {
            parsed
                .get("tool")
                .and_then(|v| v.get("pixi"))
                .ok_or_else(|| Error::Invalid(format!("{}: no [tool.pixi] table", target.id)))?
        } else {
            &parsed
        };
        let registries = BTreeMap::from([
            (
                "conda".to_owned(),
                RegistryConfig::PixiSearch {
                    manifest: target.manifest.clone(),
                },
            ),
            (
                "pypi".to_owned(),
                RegistryConfig::PypiSimple {
                    indexes: crate::pypi::index_urls(pixi),
                },
            ),
        ]);
        Ok(AvailabilityConfig {
            registries,
            exclude_newer: ["workspace", "project"]
                .iter()
                .find_map(|table| pixi.get(table)?.get("exclude-newer"))
                .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned)),
        })
    }
    fn prepare(&self, stage: &Path, target: &Target, options: &UpdateOptions) -> Result<Candidate> {
        let manifest = stage.join(&target.manifest);
        let content = fs::read_to_string(&manifest)?;
        let parsed: toml::Value = content
            .parse()
            .map_err(|e| Error::Invalid(format!("invalid manifest: {e}")))?;
        validate_paths(&parsed, manifest.parent().unwrap(), stage)?;
        let layout = crate::working_tree::Layout::new([&self.spec()]);
        for relative in crate::working_tree::files(stage, &layout)? {
            if relative
                .file_name()
                .is_some_and(|n| n == "pyproject.toml" || n == "pixi.toml")
            {
                let path = stage.join(relative);
                let value: toml::Value = fs::read_to_string(&path)?
                    .parse()
                    .map_err(|e| Error::Invalid(format!("invalid staged manifest: {e}")))?;
                validate_paths(&value, path.parent().unwrap(), stage)?;
            }
        }
        if options.upgrade && options.packages.is_empty() {
            return Err(Error::Invalid(
                "constraint changes require explicitly selected direct packages".into(),
            ));
        }
        let lock = target.manifest.parent().unwrap().join("pixi.lock");
        let before_text = fs::read_to_string(stage.join(&lock)).ok();
        let before = before_text
            .as_deref()
            .map(crate::inventory::pixi_inventory)
            .transpose()?
            .unwrap_or_default();
        // Source metadata can require a solve environment even for a lock-only
        // update. Let Pixi create it inside this disposable stage; forcing
        // --no-install rejects editable/dynamic PyPI dependencies.
        let mut args = vec![
            if options.upgrade { "upgrade" } else { "update" }.into(),
            "--manifest-path".into(),
            manifest.to_string_lossy().into(),
        ];
        args.extend(options.packages.clone());
        run(&options.tool("pixi"), &args, stage, options.timeout_seconds)?;
        let after_text = fs::read_to_string(stage.join(&lock))?;
        let after = crate::inventory::pixi_inventory(&after_text)?;
        if !options.refresh_git
            && git_artifacts(&before) != git_artifacts(&after)
            && before_text.is_some()
        {
            return Err(Error::Policy(
                "backend moved Git resolutions; select --refresh-git explicitly".into(),
            ));
        }
        if !options.upgrade && fs::read_to_string(&manifest)? != content {
            return Err(Error::Policy(
                "backend unexpectedly changed the manifest".into(),
            ));
        }
        run(
            &options.tool("pixi"),
            &[
                "lock".into(),
                "--manifest-path".into(),
                manifest.to_string_lossy().into(),
                "--check".into(),
            ],
            stage,
            options.timeout_seconds,
        )?;
        if fs::read_to_string(stage.join(&lock))? != after_text {
            return Err(Error::Operation(
                "lock consistency check changed the candidate".into(),
            ));
        }
        let mut validation = vec![format!("{}: resolved and lock-consistent", target.id)];
        if options.install {
            run(
                &options.tool("pixi"),
                &[
                    "install".into(),
                    "--manifest-path".into(),
                    manifest.to_string_lossy().into(),
                    "--locked".into(),
                ],
                stage,
                options.timeout_seconds,
            )?;
            validation.push(format!(
                "{}: default environment installed on host",
                target.id
            ));
        }
        Ok(Candidate {
            files: vec![target.manifest.clone(), lock],
            suggestions: vec![],
            unresolved: vec![],
            before,
            baseline_available: before_text.is_some(),
            after,
            validation,
        })
    }
}
fn is_pyproject(target: &Target) -> bool {
    target
        .manifest
        .file_name()
        .is_some_and(|n| n == "pyproject.toml")
}

fn git_artifacts(packages: &[crate::Package]) -> std::collections::BTreeSet<String> {
    packages
        .iter()
        .filter(|p| p.artifact.starts_with("git+") || p.artifact.contains(".git"))
        .map(|p| p.artifact.clone())
        .collect()
}

/// The ecosystem of the entries of a Pixi dependency table named `key`.
fn table_ecosystem(key: &str) -> Option<&'static str> {
    match key {
        "dependencies" | "host-dependencies" | "build-dependencies" => Some("conda"),
        "pypi-dependencies" => Some("pypi"),
        _ => None,
    }
}

/// Declarations in Pixi dependency tables, including features and platform
/// targets, in key order. `path` is the table path of `value`. Path, Git and
/// other specs without a version string have an empty requirement.
fn table_declarations(value: &toml::Value, path: &str, file: &Path, output: &mut Vec<Declaration>) {
    for (key, val) in value.as_table().into_iter().flatten() {
        let location = format!("{path}{key}");
        let Some(ecosystem) = table_ecosystem(key) else {
            table_declarations(val, &format!("{location}."), file, output);
            continue;
        };
        for (name, spec) in val.as_table().into_iter().flatten() {
            let version = spec.as_str().or_else(|| spec.get("version")?.as_str());
            output.push(Declaration {
                ecosystem: ecosystem.into(),
                package: name.clone(),
                requirement: version.unwrap_or_default().into(),
                file: file.into(),
                location: location.clone(),
            });
        }
    }
}

/// Every direct dependency declared in the manifest `text`: Pixi tables
/// first, then the standard `[project]` and `[dependency-groups]` requirements
/// of a `pyproject.toml` target. Direct URLs count as unversioned.
fn manifest_declarations(text: &str, pyproject: bool, file: &Path) -> Result<Vec<Declaration>> {
    let parsed: toml::Value = text
        .parse()
        .map_err(|e| Error::Invalid(format!("invalid manifest: {e}")))?;
    let mut output = vec![];
    if !pyproject {
        table_declarations(&parsed, "", file, &mut output);
        return Ok(output);
    }
    if let Some(pixi) = parsed.get("tool").and_then(|t| t.get("pixi")) {
        table_declarations(pixi, "tool.pixi.", file, &mut output);
    }
    output.extend(crate::pyproject::declarations(text, file, &[])?);
    Ok(output)
}

fn rewrite_tables(
    table: &mut dyn toml_edit::TableLike,
    path: &str,
    change: &mut crate::pyproject::Change,
) {
    for (key, item) in table.iter_mut() {
        let location = format!("{path}{}", key.get());
        let Some(children) = item.as_table_like_mut() else {
            continue;
        };
        if table_ecosystem(key.get()).is_none() {
            rewrite_tables(children, &format!("{location}."), change);
            continue;
        }
        for (dependency, spec) in children.iter_mut() {
            let value = if spec.is_str() {
                spec.as_value_mut()
            } else {
                spec.as_table_like_mut()
                    .and_then(|t| t.get_mut("version"))
                    .and_then(|v| v.as_value_mut())
                    .filter(|v| v.is_str())
            };
            let Some(value) = value else {
                continue;
            };
            let old = value.as_str().unwrap().to_owned();
            if let Some(new) = change(&location, dependency.get(), &old) {
                crate::pyproject::replace_string(value, &new);
            }
        }
    }
}

/// Apply `edits` to the manifest `text`, keeping comments and layout. Every
/// edit must name a declaration [`manifest_declarations`] reports for `text`.
fn rewrite_manifest(text: &str, pyproject: bool, edits: &[Edit]) -> Result<String> {
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| Error::Invalid(format!("invalid manifest: {e}")))?;
    let mut pending: Vec<&Edit> = edits.iter().collect();
    {
        let mut change = |location: &str, package: &str, old: &str| {
            let index = pending.iter().position(|e| {
                let d = &e.declaration;
                d.location == location && d.package == package && d.requirement == old
            })?;
            Some(pending.swap_remove(index).requirement.clone())
        };
        if !pyproject {
            rewrite_tables(document.as_table_mut(), "", &mut change);
        } else {
            if let Some(pixi) = document
                .get_mut("tool")
                .and_then(|t| t.get_mut("pixi"))
                .and_then(|p| p.as_table_like_mut())
            {
                rewrite_tables(pixi, "tool.pixi.", &mut change);
            }
            crate::pyproject::rewrite(&mut document, &mut change, &[]);
        }
    }
    if let Some(edit) = pending.first() {
        return Err(crate::adapter::undeclared(&edit.declaration));
    }
    Ok(document.to_string())
}

fn validate_paths(value: &toml::Value, base: &Path, stage: &Path) -> Result<()> {
    // Compare canonical spellings on both sides (e.g. macOS /var -> /private/var).
    let stage = &crate::working_tree::canonical(stage)?;
    match value {
        toml::Value::Table(t) => {
            for (key, v) in t {
                if key == "path" {
                    if let Some(p) = v.as_str() {
                        let resolved =
                            crate::working_tree::canonical(&base.join(p)).map_err(|_| {
                                Error::Invalid(format!("local path unavailable in stage: {p}"))
                            })?;
                        if !resolved.starts_with(stage) {
                            return Err(Error::Invalid(format!(
                                "local dependency escapes the repository: {p}"
                            )));
                        }
                    }
                }
                validate_paths(v, base, stage)?;
            }
        }
        toml::Value::Array(a) => {
            for v in a {
                validate_paths(v, base, stage)?;
            }
        }
        toml::Value::String(s) if s.contains("file:") => {
            return Err(Error::Invalid("file: dependency URLs are not relocatable; use a repository-relative path declaration".into()));
        }
        toml::Value::String(s) => {
            if let Some((_, location)) = s.split_once(" @ ") {
                if location.starts_with('.')
                    || location.starts_with('/')
                    || location.as_bytes().get(1) == Some(&b':')
                {
                    let resolved =
                        crate::working_tree::canonical(&base.join(location)).map_err(|_| {
                            Error::Invalid("dependency reference is unavailable in stage".into())
                        })?;
                    if !resolved.starts_with(stage) {
                        return Err(Error::Invalid("dependency reference escapes stage".into()));
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn declared(file: &str, manifest: &str) -> Vec<Declaration> {
        manifest_declarations(manifest, file == "pyproject.toml", Path::new(file)).unwrap()
    }

    /// Declarations whose requirement can exclude newer releases, as
    /// (package, requirement, ecosystem), without duplicates.
    fn capped(file: &str, manifest: &str) -> Vec<(String, String, String)> {
        let mut output = vec![];
        for d in declared(file, manifest) {
            let scheme = crate::ecosystem::scheme(&d.ecosystem).unwrap();
            let key = (d.package, d.requirement, d.ecosystem);
            if !key.1.is_empty() && scheme.caps_newer(&key.1) && !output.contains(&key) {
                output.push(key);
            }
        }
        output
    }

    /// Rewrite every versioned declaration of `name` with `change`.
    fn rewritten(
        file: &str,
        manifest: &str,
        name: &str,
        change: impl Fn(&str) -> String,
    ) -> (String, Vec<Declaration>) {
        let edits: Vec<Edit> = declared(file, manifest)
            .into_iter()
            .filter(|d| {
                !d.requirement.is_empty()
                    && if d.ecosystem == "pypi" {
                        pypi_key(&d.package) == pypi_key(name)
                    } else {
                        d.package.eq_ignore_ascii_case(name)
                    }
            })
            .map(|d| Edit {
                requirement: change(&d.requirement),
                declaration: d,
                comment: None,
            })
            .collect();
        let text = rewrite_manifest(manifest, file == "pyproject.toml", &edits).unwrap();
        (text, edits.into_iter().map(|e| e.declaration).collect())
    }

    #[test]
    fn accepted_requirements_keep_comments_layout_and_ecosystem() {
        let manifest = r#"[dependencies]
ruff = "==0.15.22"      # pinned due to ruff syntax/rule updates
[feature.lint.dependencies]
ruff = { version = "==0.15.22", channel = "conda-forge" }
[pypi-dependencies]
Six = "==1.15.0" # keep
local = { path = "local", editable = true }
"#;
        let (text, edited) = rewritten("pixi.toml", manifest, "RUFF", |old| format!("{old}.1"));
        let places: Vec<_> = edited
            .iter()
            .map(|d| (d.ecosystem.as_str(), d.location.as_str()))
            .collect();
        assert_eq!(
            places,
            [
                ("conda", "dependencies"),
                ("conda", "feature.lint.dependencies")
            ]
        );
        assert_eq!(
            text,
            manifest
                .replace(r#"ruff = "==0.15.22"  "#, r#"ruff = "==0.15.22.1"  "#)
                .replace(r#"version = "==0.15.22""#, r#"version = "==0.15.22.1""#)
        );
        let (text, edited) = rewritten("pixi.toml", manifest, "six", |_| "==1.17.0".into());
        assert_eq!(edited[0].ecosystem, "pypi");
        assert!(text.contains("Six = \"==1.17.0\" # keep\n"), "{text}");
        let local = declared("pixi.toml", manifest)
            .into_iter()
            .find(|d| d.package == "local")
            .unwrap();
        assert_eq!(local.requirement, "");
        let pyproject = r#"[project]
dependencies = [
  "Six[socks] (==1.15.0) ; python_version < '3.12'", # keep
  'urllib3>=1.26,<2',
  "direct @ https://example.invalid/direct-1.0.tar.gz",
]
[project.optional-dependencies]
extra = ["six==1.15.0"]
[dependency-groups]
dev = ["six ==1.15.0", { include-group = "extra" }]
"#;
        let (text, edited) = rewritten("pyproject.toml", pyproject, "six", |old| {
            assert_eq!(old, "==1.15.0");
            "==1.17.0".into()
        });
        let locations: Vec<_> = edited.iter().map(|d| d.location.as_str()).collect();
        assert_eq!(
            locations,
            [
                "project.dependencies[0]",
                "project.optional-dependencies.extra[0]",
                "dependency-groups.dev[0]"
            ]
        );
        assert_eq!(text, pyproject.replace("==1.15.0", "==1.17.0"));
        let (text, _) = rewritten("pyproject.toml", pyproject, "urllib3", |_| "<3".into());
        assert!(text.contains("'urllib3<3',"), "{text}");
        let direct = declared("pyproject.toml", pyproject)
            .into_iter()
            .find(|d| d.package == "direct")
            .unwrap();
        assert_eq!(direct.requirement, "");
        let pyproject = "[tool.pixi.dependencies]\nruff = '==1' # c\n";
        let (text, edited) = rewritten("pyproject.toml", pyproject, "ruff", |_| "==2".into());
        assert_eq!(edited[0].location, "tool.pixi.dependencies");
        assert_eq!(text, "[tool.pixi.dependencies]\nruff = '==2' # c\n");
    }

    #[test]
    fn edits_must_name_a_declaration_of_the_manifest() {
        let manifest = "[dependencies]\nruff = \"==1\"\n";
        let mut declaration = declared("pixi.toml", manifest).remove(0);
        declaration.requirement = "==0".into();
        let edit = Edit {
            declaration,
            requirement: "==2".into(),
            comment: None,
        };
        let error = rewrite_manifest(manifest, false, &[edit]).unwrap_err();
        assert!(
            matches!(&error, Error::Invalid(m) if m.contains("is not declared at dependencies")),
            "{error}"
        );
    }

    /// macOS temp directories sit behind a symlink (/var -> /private/var), so the
    /// stage path and canonical dependency paths differ in spelling.
    #[cfg(unix)]
    #[test]
    fn local_paths_inside_a_symlinked_stage_are_accepted() {
        let real = tempfile::tempdir().unwrap();
        fs::create_dir(real.path().join("local")).unwrap();
        let aliases = tempfile::tempdir().unwrap();
        let alias = aliases.path().join("stage");
        std::os::unix::fs::symlink(real.path(), &alias).unwrap();
        let manifest: toml::Value = "[pypi-dependencies]\nlocal = { path = \"local\" }\n"
            .parse()
            .unwrap();
        validate_paths(&manifest, &alias, &alias).unwrap();
        let escape: toml::Value = "[pypi-dependencies]\nout = { path = \"..\" }\n"
            .parse()
            .unwrap();
        assert!(validate_paths(&escape, &alias, &alias).is_err());
    }

    #[test]
    fn pyproject_standard_requirements_are_pypi_constraints() {
        let manifest = r#"
[project]
dependencies = [
  "requests>=2,<3",
  "six==1.15.0; python_version < '3.12'",
  "urllib3[socks] (<2)",
  "floor>=1",
  "bare",
  "direct @ https://example.invalid/direct-1.0.tar.gz",
]
[project.optional-dependencies]
extra = ["compatible ~=1.4"]
[dependency-groups]
lint = ["ruff==0.15.22", { include-group = "extra" }]
[tool.pixi.dependencies]
python = "3.12.*"
"#;
        let expected = [
            ("python", "3.12.*", "conda"),
            ("requests", ">=2,<3", "pypi"),
            ("six", "==1.15.0", "pypi"),
            ("urllib3", "<2", "pypi"),
            ("compatible", "~=1.4", "pypi"),
            ("ruff", "==0.15.22", "pypi"),
        ]
        .map(|(n, r, e)| (n.to_owned(), r.to_owned(), e.to_owned()));
        assert_eq!(capped("pyproject.toml", manifest), expected);
    }

    #[test]
    fn lower_bounds_alone_never_block_newer_releases() {
        let manifest = r#"
[dependencies]
floor = ">=4.0.0"
strict = ">3"
excluded = "!=1.2"
any = "*"
ceiling = "<=12.9.0"
exact = "==5.1.0"
bare = "2.17"
prefix = "1.2.*"
conda-fuzzy = "=1.2"
range = ">=3.8,<4"
open-alternative = "1.0|>=2"
bounded-alternatives = "1.0|2.*"
[pypi-dependencies]
compatible = "~=1.4"
table-floor = { version = ">=3.8" }
table-ceiling = { version = "<2" }
"#;
        let mut names: Vec<String> = capped("pixi.toml", manifest)
            .into_iter()
            .map(|(name, _, _)| name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "bare",
                "bounded-alternatives",
                "ceiling",
                "compatible",
                "conda-fuzzy",
                "exact",
                "prefix",
                "range",
                "table-ceiling"
            ]
        );
    }
}

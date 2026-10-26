//! Security updates (ADR 0004): the packages a target's actionable findings
//! move, each toward its fix version, and whether the candidate fixed them.
use crate::{
    adapter::{pypi_key, Adapter, Capabilities, Support},
    constraints::Declaration,
    graph::LockGraph,
    scan::{self, Finding, FixState, ScanReport},
    DependencyChange, Error, FixOutcome, Package, PreciseUpdate, Result, SecurityFix,
    SecurityTarget, Suggestion, Target, UpdateOptions,
};
use std::cmp::Ordering;

/// One package a security update moves.
struct Move {
    /// Ecosystem of the package.
    ecosystem: String,
    /// The package as the target declares it.
    declared: String,
    /// The release it aims at, which fixes every one of its findings.
    fix_version: String,
    /// The actionable findings it fixes.
    findings: Vec<Finding>,
    /// Whether the target does not declare it.
    transitive: bool,
    /// For a transitive package, the dependency paths from each declared
    /// dependency that reaches it; empty for a declared one, or when no
    /// path is known.
    paths: Vec<Vec<String>>,
}

/// What a security update of one target moves, from its baseline scan.
pub(crate) struct Selection {
    /// The scan of the target's current lock.
    pub(crate) baseline: ScanReport,
    moves: Vec<Move>,
    /// Findings the update reports without moving anything for them.
    pub(crate) reported: Vec<SecurityFix>,
    /// Suggestions for fixes a declared constraint blocks.
    pub(crate) suggestions: Vec<Suggestion>,
    /// Notes for the proposal, such as an aim that could not be honoured.
    pub(crate) notes: Vec<String>,
}

/// The ecosystem part of a finding's `ecosystem:name:platform` identity.
fn ecosystem_of(finding: &Finding) -> &str {
    finding.identity.split(':').next().unwrap_or_default()
}

/// The platform part of a finding's `ecosystem:name:platform` identity.
fn platform_of(finding: &Finding) -> &str {
    finding.identity.rsplit(':').next().unwrap_or_default()
}

/// Whether `left` and `right` name the same package in `ecosystem`.
pub(crate) fn same_name(ecosystem: &str, left: &str, right: &str) -> bool {
    if ecosystem == "pypi" {
        pypi_key(left) == pypi_key(right)
    } else {
        left == right
    }
}

use crate::ecosystem::compare_versions as compare;

/// The major.minor line of `version`.
fn line(version: &str) -> Vec<&str> {
    version.split('.').take(2).collect()
}

/// Whether `version` fixes `finding`: it is at least the fixed release in
/// its own major.minor line, or at least every fixed release when its line
/// has none. A version between two lines' fixes is not assumed fixed.
fn fixes(finding: &Finding, version: &str) -> bool {
    let ecosystem = ecosystem_of(finding);
    let same_line: Vec<&String> = finding
        .fix_versions
        .iter()
        .filter(|f| line(f) == line(version))
        .collect();
    if same_line.is_empty() {
        finding
            .fix_versions
            .iter()
            .all(|f| compare(ecosystem, version, f) != Ordering::Less)
    } else {
        same_line
            .iter()
            .any(|f| compare(ecosystem, version, f) != Ordering::Less)
    }
}

/// The release that fixes all of `findings` (one package's), as `aim`
/// asks: the lowest such fixed release above the locked version, else the
/// highest fixed release; for `patch`, the newest of the `available`
/// releases in that release's major.minor line that still fixes them all.
/// `None` when no fixed release is above the locked version.
fn aim_for(findings: &[&Finding], aim: SecurityTarget, available: &[String]) -> Option<String> {
    let first = findings.first()?;
    let ecosystem = ecosystem_of(first);
    let mut candidates: Vec<String> = findings
        .iter()
        .filter_map(|f| scan::fix_version(f, aim, &[]))
        .chain(findings.iter().flat_map(|f| f.fix_versions.iter().cloned()))
        .filter(|v| compare(ecosystem, v, &first.version) == Ordering::Greater)
        .collect();
    candidates.sort_by(|a, b| compare(ecosystem, a, b));
    candidates.dedup();
    let lowest = candidates
        .iter()
        .find(|v| findings.iter().all(|f| fixes(f, v)))
        .or(candidates.last())?;
    if aim != SecurityTarget::Patch {
        return Some(lowest.clone());
    }
    available
        .iter()
        .filter(|v| line(v) == line(lowest))
        .filter(|v| compare(ecosystem, v, lowest) != Ordering::Less)
        .filter(|v| findings.iter().all(|f| fixes(f, v)))
        .max_by(|a, b| compare(ecosystem, a, b))
        .or(Some(lowest))
        .cloned()
}

/// The requirement that governs package `name` in a security update: an
/// acceptance's from `accept`, else its `declaration`'s. `None` when nothing
/// constrains it here: no declaration, or a bare `--accept NAME`, which the
/// evidence decides.
fn governing_requirement(
    declaration: Option<&Declaration>,
    accept: &[String],
    ecosystem: &str,
    name: &str,
) -> Result<Option<String>> {
    let mut accepted = None;
    for text in accept {
        let acceptance = crate::constraint::parse_accept(text)?;
        if same_name(ecosystem, &acceptance.name, name) {
            accepted = Some(acceptance.requirement);
        }
    }
    Ok(match accepted {
        Some(requirement) => requirement,
        None => declaration.map(|d| d.requirement.clone()),
    })
}

fn record(
    target: &Target,
    finding: &Finding,
    fix_version: Option<String>,
    outcome: FixOutcome,
    reason: String,
) -> SecurityFix {
    SecurityFix {
        paths: vec![],
        target: target.id.clone(),
        advisory: finding.id.clone(),
        severity: finding.severity.clone(),
        ecosystem: ecosystem_of(finding).into(),
        package: finding.package.clone(),
        version: finding.version.clone(),
        fix_version,
        outcome,
        reason,
    }
}

/// A requirement on a transitive package's dependency path that excludes
/// its fix version.
struct Blocker {
    /// The package one step up the path.
    parent: String,
    /// Its recorded requirement on the transitive package.
    spec: String,
}

impl Blocker {
    /// How it holds back `name`.
    fn requirement(&self, name: &str) -> String {
        format!("{} requires {name} {}", self.parent, self.spec)
    }

    /// Why `name` cannot reach `fix_version`.
    fn reason(&self, name: &str, fix_version: &str) -> String {
        format!("{}, which excludes {fix_version}", self.requirement(name))
    }
}

/// The distinct platforms of `findings`.
fn platforms<'a>(findings: &[&'a Finding]) -> Vec<&'a str> {
    let mut platforms: Vec<&str> = vec![];
    for finding in findings {
        let platform = platform_of(finding);
        if !platforms.contains(&platform) {
            platforms.push(platform);
        }
    }
    platforms
}

/// The dependency paths from `roots` to the `ecosystem` package `name` on
/// any of `platforms`, without repeats.
fn paths_on(
    graph: &LockGraph,
    platforms: &[&str],
    roots: &[(String, String)],
    ecosystem: &str,
    name: &str,
) -> Vec<Vec<String>> {
    let mut paths: Vec<Vec<String>> = vec![];
    for platform in platforms {
        for path in graph.paths_to(platform, roots, ecosystem, name) {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

/// The requirement at the end of `path`, on the `ecosystem` package it
/// reaches, that excludes `fix_version`: only when every requirement the
/// parent records on it, at every locked version and on every one of
/// `platforms`, does. `None` when one admits the fix, or the lock records
/// none (then only the tool can tell).
fn path_blocker(
    graph: &LockGraph,
    platforms: &[&str],
    path: &[String],
    ecosystem: &str,
    fix_version: &str,
) -> Option<Blocker> {
    let scheme = crate::ecosystem::scheme(ecosystem)?;
    let [.., parent, name] = path else {
        return None;
    };
    let specs = graph
        .nodes
        .iter()
        .filter(|n| {
            platforms.contains(&n.platform.as_str()) && same_name(&n.ecosystem, &n.name, parent)
        })
        .flat_map(|n| &n.requires)
        .filter(|r| same_name(ecosystem, &r.name, name))
        .map(|r| r.spec.as_deref())
        .collect::<Option<Vec<&str>>>()?;
    let first = specs.first()?;
    specs
        .iter()
        .all(|spec| scheme.admits(spec, fix_version) == Some(false))
        .then(|| Blocker {
            parent: parent.clone(),
            spec: (*first).to_owned(),
        })
}

impl Move {
    /// The blocker of each of its paths in `graph`, on the platforms of
    /// `findings`.
    fn blockers(&self, graph: Option<&LockGraph>, findings: &[&Finding]) -> Vec<Option<Blocker>> {
        let platforms = platforms(findings);
        self.paths
            .iter()
            .map(|path| path_blocker(graph?, &platforms, path, &self.ecosystem, &self.fix_version))
            .collect()
    }

    /// A suggestion describing, without applying it, how to reach the fix
    /// of this transitive package that `blocker` excludes, or that the tool
    /// left at `now`: a newer release of the declared dependency its path
    /// starts from, or an override. `None` when no path names one of
    /// `declarations`.
    fn parent_suggestion(
        &self,
        target: &Target,
        declarations: &[Declaration],
        findings: &[&Finding],
        blocker: Option<&Blocker>,
        now: &str,
    ) -> Option<Suggestion> {
        let (name, fix_version) = (&self.declared, &self.fix_version);
        let path = self
            .paths
            .iter()
            .find(|p| p.len() >= 2 && blocker.is_none_or(|b| p[p.len() - 2] == b.parent))?;
        let introducer = path.first()?;
        let declaration = declarations.iter().find(|d| &d.package == introducer)?;
        let advisories: Vec<&str> = findings.iter().map(|f| f.id.as_str()).collect();
        let held = match blocker {
            Some(blocker) => blocker.requirement(name),
            None => format!("{name} stays at {now}"),
        };
        Some(Suggestion {
            target: target.id.clone(),
            package: declaration.package.clone(),
            requirement: declaration.requirement.clone(),
            reason: format!(
                "{} {} fixed in {name} {fix_version}, but {held} (through {}); a release of {} whose dependencies admit {fix_version}, or an override, is needed, which depsmith does not apply",
                advisories.join(", "),
                if advisories.len() == 1 { "is" } else { "are" },
                path.join(" > "),
                declaration.package
            ),
            evidence: findings
                .iter()
                .map(|f| format!("{} ({})", f.id, f.namespace))
                .collect(),
        })
    }
}

/// The suggestion for a fix that `declaration` holds back at `now`.
fn blocked(
    target: &Target,
    declaration: &Declaration,
    findings: &[&Finding],
    fix_version: &str,
    now: &str,
) -> Suggestion {
    let advisories: Vec<&str> = findings.iter().map(|f| f.id.as_str()).collect();
    Suggestion {
        target: target.id.clone(),
        package: declaration.package.clone(),
        requirement: declaration.requirement.clone(),
        reason: format!(
            "{} {} fixed in {fix_version}, but {} keeps {} at {now}; accept a requirement that admits {fix_version} with --accept {}=REQUIREMENT",
            advisories.join(", "),
            if advisories.len() == 1 { "is" } else { "are" },
            declaration.requirement,
            declaration.package,
            declaration.package
        ),
        evidence: findings
            .iter()
            .map(|f| format!("{} ({})", f.id, f.namespace))
            .collect(),
    }
}

/// Scan the current lock of `target` under `root` and choose what a
/// security update moves: each package with actionable findings, toward a
/// fix version that fixes all of them. A transitive package moves only
/// where the adapter's `capabilities` show its tool moving one. A fix that a
/// declaration, or a requirement on every one of the package's dependency
/// paths, excludes is blocked and suggested instead; unfixable findings are
/// reported.
pub(crate) fn select(
    adapter: &dyn Adapter,
    capabilities: &Capabilities,
    root: &std::path::Path,
    target: &Target,
    options: &UpdateOptions,
) -> Result<Selection> {
    let inventory = adapter.inventory(root, target)?;
    let baseline = scan::scan_pair(&target.id, None, &inventory, options, root)?;
    let actionable = scan::actionable(&baseline.findings, options.min_severity.as_deref())?;
    let rank = options
        .min_severity
        .as_deref()
        .and_then(scan::severity_rank);
    let mut selection = Selection {
        moves: vec![],
        reported: vec![],
        suggestions: vec![],
        notes: vec![],
        baseline: baseline.clone(),
    };
    for finding in &baseline.findings {
        if finding.suppression.is_none() && finding.meets(rank) && !finding.fixable() {
            let reason = match finding.fix_state {
                FixState::WontFix => "its maintainers will not fix it",
                _ => "no release fixes it",
            };
            selection.reported.push(record(
                target,
                finding,
                None,
                FixOutcome::Unfixable,
                reason.into(),
            ));
        }
    }
    // `patch` needs the releases the registry publishes, and only a precise
    // update can aim at one; without it the tool moves as far as it can.
    let lookup = (options.security_target == SecurityTarget::Patch
        && capabilities.precise_update == Support::Supported
        && !actionable.is_empty())
    .then(|| {
        adapter
            .availability(root, target)
            .map(|config| crate::constraints::Lookup::new(root, options, config))
    });
    // One move per package, whatever its findings and platforms.
    let mut packages: Vec<(String, String, Vec<&Finding>)> = vec![];
    for finding in actionable {
        let ecosystem = ecosystem_of(finding);
        match packages
            .iter_mut()
            .find(|(e, name, _)| e == ecosystem && same_name(ecosystem, name, &finding.package))
        {
            Some((_, _, findings)) => findings.push(finding),
            None => packages.push((ecosystem.into(), finding.package.clone(), vec![finding])),
        }
    }
    let declarations = adapter.declarations(root, target)?;
    let graph = adapter.lock_graph(root, target)?;
    let mut roots: Vec<(String, String)> = vec![];
    for declaration in &declarations {
        let root = (declaration.ecosystem.clone(), declaration.package.clone());
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let moves_transitive = &capabilities.transitive_updates;
    for (ecosystem, name, findings) in packages {
        let declaration = declarations
            .iter()
            .find(|d| d.ecosystem == ecosystem && same_name(&ecosystem, &d.package, &name));
        let requirement = governing_requirement(declaration, &options.accept, &ecosystem, &name)?;
        let scheme = crate::ecosystem::scheme(&ecosystem);
        let mut available = vec![];
        if let Some(lookup) = &lookup {
            let listed = match lookup {
                Ok(lookup) => lookup
                    .releases(&ecosystem, &name)
                    .map_err(|e| e.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let problem = match listed {
                Ok(Some(releases)) if !releases.is_empty() => {
                    available = releases;
                    None
                }
                Ok(_) => Some("no release data".to_owned()),
                Err(error) => Some(format!("release listing failed: {error}")),
            };
            if let Some(problem) = problem {
                selection.notes.push(format!(
                    "{}: {name}: {problem}; --security-target patch aims at its lowest fix",
                    target.id
                ));
            }
            // Only what the governing requirement admits can be aimed at.
            if let (Some(requirement), Some(scheme)) = (&requirement, &scheme) {
                available.retain(|v| scheme.admits(requirement, v) != Some(false));
            }
        }
        let Some(fix_version) = aim_for(&findings, options.security_target, &available) else {
            for finding in findings {
                selection.reported.push(record(
                    target,
                    finding,
                    None,
                    FixOutcome::Unfixable,
                    format!("no fixed release is above {}", finding.version),
                ));
            }
            continue;
        };
        let declared = adapter
            .select(root, target, std::slice::from_ref(&name))?
            .into_values()
            .next();
        let Some(declared) = declared else {
            let m = Move {
                paths: graph.as_ref().map_or_else(Vec::new, |g| {
                    paths_on(g, &platforms(&findings), &roots, &ecosystem, &name)
                }),
                ecosystem,
                declared: name,
                fix_version,
                findings: findings.iter().copied().cloned().collect(),
                transitive: true,
            };
            // The tool must be shown to move it, and no requirement on every
            // path may exclude the fix; a path that admits it is left to the
            // tool, and the candidate scan tells.
            let blockers = m.blockers(graph.as_ref(), &findings);
            let blocker = if blockers.is_empty() || blockers.iter().any(Option::is_none) {
                None
            } else {
                blockers.into_iter().flatten().next()
            };
            let reason = match (moves_transitive, &blocker) {
                (Support::Unsupported(hint), _) => Some(format!(
                    "{} is a transitive dependency, and {hint}",
                    m.declared
                )),
                (Support::NotApplicable, _) => Some(format!(
                    "{} is a transitive dependency, which this adapter does not move",
                    m.declared
                )),
                (Support::Supported, Some(blocker)) => {
                    Some(blocker.reason(&m.declared, &m.fix_version))
                }
                (Support::Supported, None) => None,
            };
            let Some(reason) = reason else {
                if m.paths.is_empty() {
                    selection.notes.push(format!(
                        "{}: {} is a transitive dependency whose dependency path is unknown",
                        target.id, m.declared
                    ));
                }
                selection.moves.push(m);
                continue;
            };
            for finding in &findings {
                let mut fix = record(
                    target,
                    finding,
                    Some(m.fix_version.clone()),
                    FixOutcome::Blocked,
                    reason.clone(),
                );
                fix.paths = m.paths.clone();
                selection.reported.push(fix);
            }
            if let Some(blocker) = &blocker {
                selection.suggestions.extend(m.parent_suggestion(
                    target,
                    &declarations,
                    &findings,
                    Some(blocker),
                    &findings[0].version,
                ));
            }
            continue;
        };
        // A requirement that excludes the fix blocks it before any tool
        // runs; an acceptance replaces the declared one before resolving.
        let excluded = declaration.filter(|_| {
            requirement
                .as_ref()
                .zip(scheme.as_ref())
                .and_then(|(requirement, scheme)| scheme.admits(requirement, &fix_version))
                == Some(false)
        });
        if let Some(declaration) = excluded {
            let now = &findings[0].version;
            for finding in &findings {
                selection.reported.push(record(
                    target,
                    finding,
                    Some(fix_version.clone()),
                    FixOutcome::Blocked,
                    format!("{} excludes {fix_version}", declaration.requirement),
                ));
            }
            selection
                .suggestions
                .push(blocked(target, declaration, &findings, &fix_version, now));
            continue;
        }
        selection.moves.push(Move {
            transitive: false,
            paths: vec![],
            ecosystem,
            declared,
            fix_version,
            findings: findings.into_iter().cloned().collect(),
        });
    }
    Ok(selection)
}

impl Selection {
    /// Whether the update moves anything.
    pub(crate) fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }

    /// The options that move exactly the selected packages: precise
    /// updates to their fix versions, or by name with `newest`, which is
    /// what an update by name does.
    pub(crate) fn options(&self, options: &UpdateOptions) -> UpdateOptions {
        let mut options = UpdateOptions {
            scan: true,
            ..options.clone()
        };
        for m in &self.moves {
            if options.security_target == SecurityTarget::Newest {
                options.packages.push(m.declared.clone());
            } else {
                options.precise.push(PreciseUpdate {
                    package: m.declared.clone(),
                    version: m.fix_version.clone(),
                });
            }
        }
        options
    }

    /// Whether `name` (in `ecosystem`) is one the update moves.
    pub(crate) fn targets(&self, ecosystem: &str, name: &str) -> bool {
        self.moves
            .iter()
            .any(|m| m.ecosystem == ecosystem && same_name(ecosystem, &m.declared, name))
    }

    /// The `changes` of packages the update did not target, as
    /// `name before -> after`.
    pub(crate) fn untargeted(&self, changes: &[DependencyChange]) -> Vec<String> {
        let version = |p: &Option<Package>| {
            p.as_ref()
                .map_or_else(|| "none".to_owned(), |p| p.version.clone())
        };
        changes
            .iter()
            .filter_map(|change| {
                let package = change.after.as_ref().or(change.before.as_ref())?;
                (!self.targets(&package.ecosystem, &package.name)).then(|| {
                    format!(
                        "{} {} -> {}",
                        package.name,
                        version(&change.before),
                        version(&change.after)
                    )
                })
            })
            .collect()
    }

    /// Classify each targeted finding against the candidate's `report`: fixed
    /// when the candidate no longer has it, else blocked, with a suggestion
    /// naming the declaration from `declarations`, or for a transitive
    /// package the one its dependency path starts from, citing the
    /// requirement in `graph` that holds it back. Both are read from the
    /// stage. An unsuppressed finding the candidate introduces fails the
    /// update.
    pub(crate) fn verify(
        &self,
        target: &Target,
        report: &ScanReport,
        after: &[Package],
        declarations: &[Declaration],
        graph: Option<&LockGraph>,
    ) -> Result<(Vec<SecurityFix>, Vec<Suggestion>)> {
        let introduced: Vec<String> = report
            .introduced
            .iter()
            .filter(|f| f.suppression.is_none())
            .map(|f| format!("{} in {} {}", f.id, f.package, f.version))
            .collect();
        if !introduced.is_empty() {
            return Err(Error::Policy(format!(
                "the security update introduced findings: {}",
                introduced.join(", ")
            )));
        }
        let mut fixes = vec![];
        let mut suggestions = vec![];
        for m in &self.moves {
            let remains = |f: &Finding| {
                report
                    .findings
                    .iter()
                    .any(|c| c.id == f.id && c.namespace == f.namespace && c.identity == f.identity)
            };
            let still: Vec<&Finding> = m.findings.iter().filter(|f| remains(f)).collect();
            // The version on the platform where the finding remains.
            let now = still
                .first()
                .and_then(|f| {
                    after.iter().find(|p| {
                        p.ecosystem == m.ecosystem
                            && p.platform == platform_of(f)
                            && same_name(&m.ecosystem, &p.name, &m.declared)
                    })
                })
                .map_or("unknown", |p| p.version.as_str());
            // Any path's requirement explains a package that stayed.
            let blocker = m.blockers(graph, &still).into_iter().flatten().next();
            for finding in &m.findings {
                let blocked = remains(finding);
                let mut fix = record(
                    target,
                    finding,
                    Some(m.fix_version.clone()),
                    if blocked {
                        FixOutcome::Blocked
                    } else {
                        FixOutcome::Fixed
                    },
                    match (blocked, &blocker) {
                        (false, _) => String::new(),
                        (true, Some(blocker)) => blocker.reason(&m.declared, &m.fix_version),
                        (true, None) if m.transitive && m.paths.is_empty() => format!(
                            "{} stays at {now}, below the fix version {}, and its dependency path is unknown",
                            m.declared, m.fix_version
                        ),
                        (true, None) => format!(
                            "{} stays at {now}, below the fix version {}",
                            m.declared, m.fix_version
                        ),
                    },
                );
                fix.paths = m.paths.clone();
                fixes.push(fix);
            }
            if still.is_empty() {
                continue;
            }
            if m.transitive {
                suggestions.extend(m.parent_suggestion(
                    target,
                    declarations,
                    &still,
                    blocker.as_ref(),
                    now,
                ));
                continue;
            }
            if let Some(declaration) = declarations.iter().find(|d| {
                d.ecosystem == m.ecosystem && same_name(&m.ecosystem, &d.package, &m.declared)
            }) {
                suggestions.push(blocked(target, declaration, &still, &m.fix_version, now));
            }
        }
        Ok((fixes, suggestions))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(version: &str, fixes: &[&str]) -> Finding {
        serde_json::from_value(serde_json::json!({"id": "GHSA-x", "namespace": "n",
            "package": "p", "version": version, "severity": "High", "evidence": [],
            "identity": "pypi:p:any", "applicability": "", "artifact": "", "suppression": null,
            "fix_versions": fixes, "fix_state": "fixed"}))
        .unwrap()
    }

    #[test]
    fn a_fix_version_fixes_every_finding_without_overshooting_a_backport() {
        let backported = finding("2.8", &["2.9", "3.1"]);
        let newer_only = finding("2.8", &["3.0"]);
        // 3.0 is between the first finding's lines: not assumed fixed.
        assert!(!fixes(&backported, "3.0"));
        assert!(fixes(&backported, "2.9.1") && fixes(&backported, "3.1"));
        assert_eq!(
            aim_for(&[&backported, &newer_only], SecurityTarget::Lowest, &[]).as_deref(),
            Some("3.1")
        );
        assert_eq!(
            aim_for(&[&backported], SecurityTarget::Lowest, &[]).as_deref(),
            Some("2.9")
        );
    }
}

//! The `depsmith` command line: argument parsing, interactive target
//! selection and confirmation, and human, Markdown or JSON reports with the
//! documented exit statuses.
use clap::{Parser, Subcommand};
use depsmith_core::{self as core, Error, Result};
use serde_json::{json, Value};
use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(
    name = "depsmith",
    version,
    about = "Review and apply dependency updates across package managers"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Repository root to work in.
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    /// Print a JSON report on stdout; diagnostics go to stderr.
    #[arg(long, global = true)]
    json: bool,
    /// Print a Markdown summary, for example for a CI job summary.
    #[arg(long, global = true)]
    markdown: bool,
    /// Never prompt; fail when a choice would be needed.
    #[arg(long, global = true)]
    non_interactive: bool,
    /// Target to work on, as `manager:path` (repeatable); see `discover`.
    #[arg(long, global = true)]
    target: Vec<String>,
    /// Select every discovered target.
    #[arg(long, global = true)]
    all: bool,
    /// Update only this direct dependency (repeatable).
    #[arg(long, global = true)]
    package: Vec<String>,
    /// Accept a constraint suggestion: NAME (same style, evidenced version) or NAME=REQUIREMENT.
    #[arg(long, global = true, value_name = "NAME[=REQUIREMENT]")]
    accept: Vec<String>,
    /// Allow the selected packages' declared constraints to change (requires --package).
    #[arg(long, global = true)]
    upgrade: bool,
    /// Allow Git pins to move to newer commits.
    #[arg(long, global = true)]
    refresh_git: bool,
    /// Scan the baseline and candidate for known vulnerabilities with Grype.
    #[arg(long, global = true)]
    scan: bool,
    /// Also install the candidate's default environment on this host.
    #[arg(long, global = true)]
    install: bool,
    /// Reject the proposal on findings of at least this severity
    /// (negligible, low, medium, high, critical); requires --scan.
    #[arg(long, global = true)]
    fail_on: Option<String>,
    /// Apply --fail-on only to findings the update introduces.
    #[arg(long, global = true)]
    only_new: bool,
    /// Minimum release age in days; rejected where the package manager cannot enforce it.
    #[arg(long, global = true)]
    cooldown_days: Option<u32>,
    /// Time limit for each package manager or scanner process [default: 300].
    #[arg(long, global = true)]
    timeout_seconds: Option<u64>,
    /// Path of a native tool, as NAME=PATH (repeatable), for example
    /// `--tool pixi=/opt/pixi/bin/pixi`; `doctor` lists the tool names.
    #[arg(long = "tool", global = true, value_name = "NAME=PATH")]
    tools: Vec<String>,
    /// Deprecated: use `--tool pixi=PATH`.
    #[arg(long, global = true)]
    pixi: Option<String>,
    /// Deprecated: use `--tool grype=PATH`.
    #[arg(long, global = true)]
    grype: Option<String>,
}
#[derive(Subcommand)]
enum Command {
    /// Check the native tools the discovered targets use, offering to
    /// install each missing one (pinned, sha256-verified) into the tool
    /// cache; exit 3 when a used tool is still missing.
    Init {
        /// Install every missing used tool without asking.
        #[arg(long)]
        fetch_tools: bool,
    },
    /// List the targets found under the root.
    Discover,
    /// Report adapter capabilities and whether native tools are available.
    Doctor,
    /// Prepare a proposal without writing; exit 1 when updates are pending.
    Check,
    /// Prepare a proposal, preview it, and apply it when confirmed.
    Update {
        /// Apply the proposal (noninteractively also needs --yes).
        #[arg(long)]
        apply: bool,
        /// Confirm applying without a prompt.
        #[arg(long)]
        yes: bool,
        /// Apply the successful targets even if others failed.
        #[arg(long)]
        allow_partial: bool,
    },
    /// Scan the current locks for known vulnerabilities without updating.
    Scan,
    /// Restore the files of an interrupted apply.
    Recover,
}
fn overrides(cli: &Cli) -> Result<Value> {
    let mut values = serde_json::Map::new();
    if !cli.tools.is_empty() {
        let mut tools = serde_json::Map::new();
        for entry in &cli.tools {
            let (name, path) = entry
                .split_once('=')
                .filter(|(name, path)| !name.is_empty() && !path.is_empty())
                .ok_or_else(|| {
                    Error::Invalid(format!("--tool expects NAME=PATH, got {entry:?}"))
                })?;
            tools.insert(name.into(), json!(path));
        }
        values.insert("tools".into(), Value::Object(tools));
    }
    for (name, enabled) in [
        ("upgrade", cli.upgrade),
        ("refresh_git", cli.refresh_git),
        ("scan", cli.scan),
        ("install", cli.install),
        ("only_new", cli.only_new),
    ] {
        if enabled {
            values.insert(name.into(), json!(true));
        }
    }
    if !cli.package.is_empty() {
        values.insert("packages".into(), json!(cli.package));
    }
    if !cli.accept.is_empty() {
        values.insert("accept".into(), json!(cli.accept));
    }
    for (key, value) in [
        ("pixi", &cli.pixi),
        ("grype", &cli.grype),
        ("fail_on", &cli.fail_on),
    ] {
        if let Some(value) = value {
            values.insert(key.into(), json!(value));
        }
    }
    if cli.fail_on.is_some() || matches!(cli.command, Command::Scan) {
        values.insert("scan".into(), json!(true));
    }
    if let Some(value) = cli.cooldown_days {
        values.insert("cooldown_days".into(), json!(value));
    }
    if let Some(value) = cli.timeout_seconds {
        values.insert("timeout_seconds".into(), json!(value));
    }
    Ok(Value::Object(values))
}
fn confirm(message: &str) -> Result<bool> {
    eprint!("{message} [y/N] ");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
/// Ask about each discovered target, then offer to save a non-empty selection
/// as `targets` in `depsmith.toml`.
fn select_interactively(
    root: &Path,
    targets: &[core::Target],
    prompt: &mut dyn FnMut(&str) -> Result<bool>,
) -> Result<Vec<String>> {
    let mut selected = vec![];
    for target in targets {
        if prompt(&format!("Select {}?", target.id))? {
            selected.push(target.id.clone());
        }
    }
    if !selected.is_empty() && prompt("Save this selection to depsmith.toml?")? {
        core::config::save_targets(root, &selected)?;
    }
    Ok(selected)
}
/// Whether to install the missing `tool` during `init`: always with
/// `--fetch-tools`, after asking when interactive, never otherwise.
fn consent(
    tool: &core::adapter::ToolSpec,
    reason: &str,
    fetch_tools: bool,
    interactive: bool,
    prompt: &mut dyn FnMut(&str) -> Result<bool>,
) -> Result<bool> {
    if fetch_tools {
        return Ok(true);
    }
    if !interactive {
        return Ok(false);
    }
    let cache = core::provision::cache_dir()
        .map_or_else(|| "the tool cache".to_owned(), |d| d.display().to_string());
    let version = tool.pinned_version().unwrap_or("");
    let name = &tool.name;
    prompt(&format!(
        "{name}: {reason}. Download {name} {version} (sha256-verified) into {cache}?"
    ))
}
fn execute(cli: &Cli) -> Result<(Value, u8)> {
    if cli.json && cli.markdown {
        return Err(Error::Invalid("choose JSON or Markdown output".into()));
    }
    let config = core::config::settings(&cli.root, &overrides(cli)?)?;
    if matches!(cli.command, Command::Doctor) {
        return Ok((core::doctor(&cli.root, &config.options), 0));
    }
    if let Command::Init { fetch_tools } = cli.command {
        for (used, flag) in [
            (!cli.package.is_empty(), "--package"),
            (!cli.accept.is_empty(), "--accept"),
            (cli.upgrade, "--upgrade"),
            (cli.refresh_git, "--refresh-git"),
            (cli.install, "--install"),
        ] {
            if used {
                return Err(Error::Invalid(format!("{flag} is not used by init")));
            }
        }
        if cli.all && !cli.target.is_empty() {
            return Err(Error::Invalid("choose --all or --target".into()));
        }
        // Prompts go to stderr, so they work alongside --json.
        let interactive = io::stdin().is_terminal() && !cli.non_interactive;
        let report = core::init(
            &cli.root,
            &cli.target,
            &config.options,
            &mut |tool, reason| consent(tool, reason, fetch_tools, interactive, &mut confirm),
        )?;
        let missing = report["missing"].as_array().is_some_and(|m| !m.is_empty());
        return Ok((report, if missing { 3 } else { 0 }));
    }
    if matches!(cli.command, Command::Recover) {
        return Ok((
            json!({"schema_version":1, "restored":core::recover(&cli.root)?}),
            0,
        ));
    }
    let targets = core::discover(&cli.root)?;
    if matches!(cli.command, Command::Discover) {
        return Ok((json!({"schema_version":1,"targets":targets}), 0));
    }
    if cli.all && !cli.target.is_empty() {
        return Err(Error::Invalid("choose --all or --target".into()));
    }
    let mut selected = if cli.all {
        targets.iter().map(|t| t.id.clone()).collect()
    } else if !cli.target.is_empty() {
        cli.target.clone()
    } else {
        config.targets
    };
    let interactive = io::stdin().is_terminal() && !cli.non_interactive && !cli.json;
    if selected.is_empty() && interactive {
        selected = select_interactively(&cli.root, &targets, &mut confirm)?;
    }
    if selected.is_empty() {
        return Err(Error::Invalid(
            "no targets selected; pass --target, --all, or configure targets".into(),
        ));
    }
    let options = config.options;
    if matches!(cli.command, Command::Scan) {
        let reports = core::scan_existing(&cli.root, &selected, &options)?;
        let status = if reports.iter().all(|r| r.policy_passed) {
            0
        } else {
            4
        };
        return Ok((json!({"schema_version":1, "scans":reports}), status));
    }
    let proposal = core::Engine::default().prepare(&cli.root, &selected, options)?;
    let mut status = proposal.exit_code(matches!(cli.command, Command::Check));
    let mut applied = None;
    if let Command::Update {
        apply,
        yes,
        allow_partial,
    } = cli.command
    {
        if interactive {
            eprintln!(
                "{}",
                render(&serde_json::to_value(&proposal).unwrap(), false)
            );
            for change in &proposal.changes {
                eprintln!("{}", change.diff);
            }
        }
        let approved = if apply {
            if !yes && !interactive {
                return Err(Error::Invalid(
                    "noninteractive application requires --apply --yes".into(),
                ));
            }
            yes || confirm("Apply these exact changes?")?
        } else {
            interactive && !proposal.changes.is_empty() && confirm("Apply these exact changes?")?
        };
        if approved {
            applied = Some(core::apply(&proposal, allow_partial)?);
            status = proposal.exit_code(false);
        }
    }
    Ok((
        json!({"schema_version":1,"proposal":proposal,"application":applied}),
        status,
    ))
}
fn render_doctor(value: &Value) -> String {
    let mut lines = vec!["Native tools".to_owned()];
    for tool in value["tools"].as_array().into_iter().flatten() {
        let tested: Vec<_> = tool["tested_versions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let detail = match tool["status"].as_str() {
            Some("unavailable") => tool["error"].as_str().unwrap_or("?").to_owned(),
            _ => format!("version {}", tool["version"].as_str().unwrap_or("unknown")),
        };
        lines.push(format!(
            "- {}: {} ({detail}; tested: {})",
            tool["tool"].as_str().unwrap_or("?"),
            tool["status"].as_str().unwrap_or("?"),
            tested.join(", ")
        ));
    }
    for adapter in value["adapters"].as_array().into_iter().flatten() {
        lines.push(format!(
            "\nAdapter {}",
            adapter["manager"].as_str().unwrap_or("?")
        ));
        for (name, support) in adapter.as_object().into_iter().flatten() {
            let Some(status) = support["status"].as_str() else {
                continue;
            };
            match support["hint"].as_str() {
                Some(hint) => lines.push(format!("- {name}: {status} ({hint})")),
                None => lines.push(format!("- {name}: {status}")),
            }
        }
    }
    lines.join("\n")
}
fn render_scans(reports: &[Value], lines: &mut Vec<String>) {
    for report in reports {
        let count = |key: &str| report[key].as_array().map_or(0, Vec::len);
        lines.push(format!(
            "- Scan {} ({}): {} findings, {} introduced, {} resolved; {} unassessed; policy {}",
            report["target"].as_str().unwrap_or("?"),
            report["comparison"].as_str().unwrap_or("?"),
            count("findings"),
            count("introduced"),
            count("resolved"),
            count("unknown_after"),
            if report["policy_passed"] == true {
                "passed"
            } else {
                "FAILED"
            },
        ));
        for finding in report["findings"].as_array().into_iter().flatten() {
            let suppressed = finding["suppression"]["reason"]
                .as_str()
                .map(|r| format!(" [suppressed: {r}]"))
                .unwrap_or_default();
            lines.push(format!(
                "  - {} {} {} {} ({}){suppressed}",
                finding["severity"].as_str().unwrap_or("?"),
                finding["id"].as_str().unwrap_or("?"),
                finding["package"].as_str().unwrap_or("?"),
                finding["version"].as_str().unwrap_or("?"),
                finding["applicability"].as_str().unwrap_or("?"),
            ));
        }
        for expired in report["expired_suppressions"]
            .as_array()
            .into_iter()
            .flatten()
        {
            lines.push(format!(
                "  - Expired suppression {} (expired {}) was not applied",
                expired["id"].as_str().unwrap_or("?"),
                expired["expires"].as_str().unwrap_or("?"),
            ));
        }
    }
}
fn render_init(value: &Value) -> String {
    let names = |key: &str| -> Vec<&str> {
        value[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect()
    };
    let mut lines = vec![format!(
        "Native tools used by {} target(s)",
        names("targets").len()
    )];
    for tool in value["tools"].as_array().into_iter().flatten() {
        let used_by: Vec<_> = tool["used_by"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let state = match tool["source"].as_str() {
            Some("missing") => "missing".to_owned(),
            Some("untrusted") => format!(
                "not trusted ({})",
                tool["untrusted"].as_str().unwrap_or("unknown reason")
            ),
            source => format!(
                "{} ({}, version {})",
                tool["status"].as_str().unwrap_or("?"),
                source.unwrap_or("?"),
                tool["version"].as_str().unwrap_or("unknown")
            ),
        };
        lines.push(format!(
            "- {}: {state}; used by {}",
            tool["tool"].as_str().unwrap_or("?"),
            used_by.join(", ")
        ));
    }
    let installed = names("installed");
    if !installed.is_empty() {
        lines.push(format!("Installed: {}", installed.join(", ")));
    }
    for failure in value["failed"].as_array().into_iter().flatten() {
        lines.push(format!(
            "Failed to install {}: {}",
            failure["tool"].as_str().unwrap_or("?"),
            failure["error"].as_str().unwrap_or("?")
        ));
    }
    let missing = names("missing");
    if !missing.is_empty() {
        lines.push(format!(
            "Still missing: {}. Install them, pass --tool NAME=PATH, or run `depsmith init --fetch-tools`.",
            missing.join(", ")
        ));
    }
    lines.join("\n")
}
fn render(value: &Value, markdown: bool) -> String {
    if value.get("installed").is_some() && value.get("tools").is_some() {
        return render_init(value);
    }
    if value.get("adapters").is_some() && value.get("tools").is_some() {
        return render_doctor(value);
    }
    if let (Some(scans), None) = (
        value.get("scans").and_then(Value::as_array),
        value.get("proposal"),
    ) {
        let mut lines = vec!["Vulnerability scan report".to_owned()];
        render_scans(scans, &mut lines);
        return lines.join("\n");
    }
    if let Some(targets) = value.get("targets").and_then(Value::as_array) {
        return targets
            .iter()
            .map(|t| t["id"].as_str().unwrap_or("?").to_owned())
            .collect::<Vec<_>>()
            .join("\n");
    }
    let p = value.get("proposal").unwrap_or(value);
    if p.get("changes").is_some() {
        let mut lines = vec![if markdown {
            "## Dependency update report".into()
        } else {
            "Dependency update report".into()
        }];
        for key in ["changes", "suggestions", "unresolved", "failures", "scans"] {
            lines.push(format!("{key}: {}", p[key].as_array().map_or(0, Vec::len)));
        }
        for change in p["changes"].as_array().into_iter().flatten() {
            lines.push(format!("- {}", change["path"].as_str().unwrap_or("?")));
        }
        for failure in p["failures"].as_array().into_iter().flatten() {
            lines.push(format!(
                "- FAILED {}: {}",
                failure["target"].as_str().unwrap_or("?"),
                failure["message"].as_str().unwrap_or("?")
            ));
        }
        for suggestion in p["suggestions"].as_array().into_iter().flatten() {
            lines.push(format!(
                "- Suggestion {} ({}): {}",
                suggestion["package"].as_str().unwrap_or("?"),
                suggestion["requirement"].as_str().unwrap_or("?"),
                suggestion["reason"].as_str().unwrap_or("?")
            ));
            for evidence in suggestion["evidence"].as_array().into_iter().flatten() {
                lines.push(format!(
                    "  - Evidence: {}",
                    evidence.as_str().unwrap_or("?")
                ));
            }
        }
        render_scans(
            p["scans"].as_array().map_or(&[][..], Vec::as_slice),
            &mut lines,
        );
        for note in p["validation"].as_array().into_iter().flatten() {
            lines.push(format!("- Validation: {}", note.as_str().unwrap_or("?")));
        }
        for entry in p["unresolved"].as_array().into_iter().flatten() {
            lines.push(format!(
                "- Unchanged {}: {}",
                entry["reference"].as_str().unwrap_or("?"),
                entry["reason"].as_str().unwrap_or("?")
            ));
        }
        if markdown {
            for change in p["changes"].as_array().into_iter().flatten() {
                lines.push(format!(
                    "\n```diff\n{}```",
                    change["diff"].as_str().unwrap_or("")
                ));
            }
        }
        lines.join("\n")
    } else {
        serde_json::to_string_pretty(value).unwrap()
    }
}
/// Run the CLI with `args` (including the program name) and return the exit
/// status: 0 success or no pending changes, 1 pending changes found by
/// `check`, 2 invalid request, 3 tool or scanner failure, 4 policy rejection,
/// 5 partial success, 130 interrupted.
///
/// Installs interrupt handlers that cancel running package managers, so call
/// it only from a process the CLI owns, never from a library host.
pub fn run_from(args: Vec<String>) -> u8 {
    // The CLI owns its process, so interrupts may cancel backend process trees.
    core::process::install_cancellation_handlers();
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code() as u8;
            if code != 0 && args.iter().any(|arg| arg == "--json") {
                println!("{}", json!({"schema_version":1,"error":error.to_string()}));
            }
            let _ = error.print();
            return code;
        }
    };
    let (value, status) = match execute(&cli) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("{error}");
            (
                json!({"schema_version":1,"error":error.to_string()}),
                error.exit_code(),
            )
        }
    };
    if cli.json {
        println!("{}", serde_json::to_string(&value).unwrap());
    } else {
        println!("{}", render(&value, cli.markdown));
    }
    if core::process::cancelled() {
        eprintln!("cancelled");
        return 130;
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn targets(ids: &[&str]) -> Vec<core::Target> {
        ids.iter()
            .map(|id| core::Target {
                id: id.to_string(),
                manager: "pixi".into(),
                manifest: id.split_once(':').unwrap().1.into(),
            })
            .collect()
    }

    /// Answers prompts in order and records the questions asked.
    fn scripted<'a>(
        answers: &'a [bool],
        asked: &'a mut Vec<String>,
    ) -> impl FnMut(&str) -> Result<bool> + 'a {
        let mut answers = answers.iter();
        move |question| {
            asked.push(question.to_owned());
            Ok(*answers.next().expect("unexpected prompt"))
        }
    }

    fn tool(name: &str, downloadable: bool) -> core::adapter::ToolSpec {
        core::adapter::ToolSpec {
            name: name.into(),
            default: name.into(),
            tested_versions: vec!["1.0.0".into()],
            downloads: if downloadable {
                vec![core::provision::ToolDownload {
                    os: std::env::consts::OS.into(),
                    arch: std::env::consts::ARCH.into(),
                    url: format!("https://example.invalid/{name}"),
                    sha256: "0".repeat(64),
                    archive: core::provision::Archive::Binary,
                    executable: name.into(),
                }]
            } else {
                vec![]
            },
        }
    }

    #[test]
    fn init_installs_with_the_flag_or_a_confirmed_prompt_only() {
        let uv = tool("uv", true);
        let mut asked = vec![];
        let ask = |fetch, interactive, answers: &[bool], asked: &mut Vec<String>| {
            consent(
                &uv,
                "not installed",
                fetch,
                interactive,
                &mut scripted(answers, asked),
            )
            .unwrap()
        };
        assert!(ask(true, true, &[], &mut asked));
        assert!(!ask(false, false, &[], &mut asked));
        assert!(asked.is_empty());
        assert!(ask(false, true, &[true], &mut asked));
        assert!(!ask(false, true, &[false], &mut asked));
        assert_eq!(asked.len(), 2);
        assert!(
            asked[0].starts_with("uv: not installed. Download uv 1.0.0 (sha256-verified)"),
            "{}",
            asked[0]
        );
        let failing = consent(&uv, "not installed", false, true, &mut |_| {
            Err(Error::Invalid("no terminal".into()))
        });
        assert!(failing.is_err());
    }

    #[test]
    fn interactive_selection_is_saved_only_when_confirmed() {
        let root = tempfile::tempdir().unwrap();
        let found = targets(&["pixi:a/pixi.toml", "pixi:b/pixi.toml"]);
        let mut asked = vec![];
        let selected = select_interactively(
            root.path(),
            &found,
            &mut scripted(&[false, true, true], &mut asked),
        )
        .unwrap();
        assert_eq!(selected, ["pixi:b/pixi.toml"]);
        assert_eq!(
            asked,
            [
                "Select pixi:a/pixi.toml?",
                "Select pixi:b/pixi.toml?",
                "Save this selection to depsmith.toml?",
            ]
        );
        let saved = core::config::settings(root.path(), &json!({})).unwrap();
        assert_eq!(saved.targets, ["pixi:b/pixi.toml"]);

        let other = tempfile::tempdir().unwrap();
        let mut asked = vec![];
        select_interactively(
            other.path(),
            &found,
            &mut scripted(&[true, false, false], &mut asked),
        )
        .unwrap();
        assert!(!other.path().join("depsmith.toml").exists());
    }

    #[test]
    fn declining_every_target_asks_nothing_about_saving() {
        let root = tempfile::tempdir().unwrap();
        let mut asked = vec![];
        let selected = select_interactively(
            root.path(),
            &targets(&["pixi:pixi.toml"]),
            &mut scripted(&[false], &mut asked),
        )
        .unwrap();
        assert!(selected.is_empty());
        assert_eq!(asked.len(), 1);
        assert!(fs::read_dir(root.path()).unwrap().next().is_none());
    }
}

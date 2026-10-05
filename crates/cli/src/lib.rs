//! The `depsmith` command line: argument parsing, interactive target
//! selection and confirmation, and human, Markdown or JSON reports with the
//! documented exit statuses.
use clap::{Parser, Subcommand};
use depsmith_core::{self as core, Error, Result};
use serde_json::{json, Value};
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
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
    /// Accept a suggestion: NAME (same style, evidenced version; for GitHub Actions the newest major, else a commit pin) or NAME=REQUIREMENT.
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
    /// Choose targets and save them to depsmith.toml, offer to install each missing native tool they use (pinned,
    /// sha256-verified) into the tool cache, and report what their adapters
    /// support; exit 3 when a used tool is still missing.
    Init {
        /// Install every missing used tool without asking.
        #[arg(long)]
        fetch_tools: bool,
        /// Do not write the selection to depsmith.toml.
        #[arg(long)]
        no_save: bool,
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
/// A target selection: the saved targets still found, plus those chosen now.
#[derive(Debug, Default)]
struct Choice {
    /// Every selected target: the saved ones still found, then those added.
    selected: Vec<String>,
    /// Discovered targets chosen now.
    added: Vec<String>,
    /// Discovered targets neither saved nor chosen.
    unselected: Vec<String>,
    /// Saved targets that are no longer found.
    stale: Vec<String>,
}

/// Extend the `saved` selection with discovered targets: the `requested`
/// ones (`--all` or `--target`), or else, interactively, those chosen when
/// the new targets are offered by manager, then by file when a manager's are
/// declined. Otherwise nothing is chosen and the new targets are unselected.
///
/// # Errors
///
/// Returns [`Error::Invalid`] for a requested target that was not found, and
/// any error from `prompt`.
fn choose_targets(
    found: &[core::Target],
    saved: &[String],
    requested: Option<&[String]>,
    interactive: bool,
    prompt: &mut dyn FnMut(&str) -> Result<bool>,
) -> Result<Choice> {
    let is_found = |id: &String| found.iter().any(|t| &t.id == id);
    let (current, stale): (Vec<String>, Vec<String>) =
        saved.iter().cloned().partition(|id| is_found(id));
    let mut choice = Choice {
        selected: current,
        stale,
        ..Choice::default()
    };
    if let Some(requested) = requested {
        if let Some(unknown) = requested.iter().find(|id| !is_found(id)) {
            return Err(Error::Invalid(format!("unknown target: {unknown}")));
        }
        choice.added = requested
            .iter()
            .filter(|id| !saved.contains(id))
            .cloned()
            .collect();
    } else {
        let unsaved: Vec<&core::Target> = found.iter().filter(|t| !saved.contains(&t.id)).collect();
        let mut managers: Vec<&str> = vec![];
        for target in &unsaved {
            if !managers.contains(&target.manager.as_str()) {
                managers.push(&target.manager);
            }
        }
        for manager in managers {
            let group: Vec<&str> = unsaved
                .iter()
                .filter(|t| t.manager == manager)
                .map(|t| t.id.as_str())
                .collect();
            if !interactive {
                choice
                    .unselected
                    .extend(group.iter().map(|id| (*id).to_owned()));
                continue;
            }
            let all = group.len() > 1
                && prompt(&format!("Select all {} {manager} targets?", group.len()))?;
            for id in group {
                if all || prompt(&format!("Select {id}?"))? {
                    choice.added.push(id.to_owned());
                } else {
                    choice.unselected.push(id.to_owned());
                }
            }
        }
    }
    choice.selected.extend(choice.added.iter().cloned());
    Ok(choice)
}
/// `depsmith init`: choose targets, save the newly chosen ones unless
/// `no_save`, then check (and with consent install) their tools and report
/// their adapters. When nothing is chosen interactively nothing is checked;
/// without a terminal or a saved selection every target is checked.
fn run_init(
    cli: &Cli,
    config: core::config::Config,
    fetch_tools: bool,
    no_save: bool,
    interactive: bool,
    prompt: &mut dyn FnMut(&str) -> Result<bool>,
) -> Result<(Value, u8)> {
    let found = core::discover(&cli.root)?;
    let requested: Option<Vec<String>> = if cli.all {
        Some(found.iter().map(|t| t.id.clone()).collect())
    } else if !cli.target.is_empty() {
        Some(cli.target.clone())
    } else {
        None
    };
    let choice = choose_targets(
        &found,
        &config.targets,
        requested.as_deref(),
        interactive,
        prompt,
    )?;
    if !no_save {
        core::config::add_targets(&cli.root, &choice.added)?;
    }
    let mut report = if choice.selected.is_empty() && interactive {
        json!({"schema_version": 1, "targets": [], "tools": [], "installed": [],
            "failed": [], "missing": [], "adapters": [], "warnings": []})
    } else {
        core::init(
            &cli.root,
            &choice.selected,
            &config.options,
            &mut |tool, reason| consent(tool, reason, fetch_tools, interactive, prompt),
        )?
    };
    report["config"] = json!({
        "path": cli.root.join("depsmith.toml"),
        "saved": !no_save,
        "added": choice.added,
        "unselected": choice.unselected,
        "stale": choice.stale,
    });
    let missing = report["missing"].as_array().is_some_and(|m| !m.is_empty());
    Ok((report, if missing { 3 } else { 0 }))
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
    if let Command::Init {
        fetch_tools,
        no_save,
    } = cli.command
    {
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
        return run_init(cli, config, fetch_tools, no_save, interactive, &mut confirm);
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
        eprintln!("No targets are saved; `depsmith init` chooses and saves them.");
        selected = choose_targets(&targets, &[], None, true, &mut confirm)?.selected;
        if !selected.is_empty() && confirm("Save this selection to depsmith.toml?")? {
            core::config::add_targets(&cli.root, &selected)?;
        }
    }
    if selected.is_empty() {
        return Err(Error::Invalid(
            "no targets selected; run `depsmith init` to choose and save them, or pass --target or --all".into(),
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
    render_adapters(value, &mut lines);
    lines.join("\n")
}
fn render_adapters(value: &Value, lines: &mut Vec<String>) {
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
    let config = &value["config"];
    let listed = |key: &str| -> Vec<&str> {
        config[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect()
    };
    let added = listed("added");
    if !added.is_empty() && config["saved"] == true {
        lines.push(format!(
            "Saved to {}: {}",
            config["path"].as_str().unwrap_or("depsmith.toml"),
            added.join(", ")
        ));
    } else if !added.is_empty() {
        lines.push(format!(
            "Chosen but not saved (--no-save): {}",
            added.join(", ")
        ));
    }
    for target in listed("stale") {
        lines.push(format!(
            "No longer found: {target} (still saved in depsmith.toml; remove it there if it is gone for good)"
        ));
    }
    for target in listed("unselected") {
        lines.push(format!(
            "Not selected: {target} (add it by running `depsmith init` in a terminal or with --target)"
        ));
    }
    for warning in names("warnings") {
        lines.push(format!("Warning: {warning}"));
    }
    render_adapters(value, &mut lines);
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
        // Packages that moved only because declared ones need them, once per
        // move with the platforms it happened on.
        let mut explained: Vec<(String, Vec<&str>)> = vec![];
        for change in p["dependencies"].as_array().into_iter().flatten() {
            let introducers: Vec<&str> = change["introducers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if introducers.is_empty() {
                continue;
            }
            let package = if change["after"].is_null() {
                &change["before"]
            } else {
                &change["after"]
            };
            let version = |side: &Value| side["version"].as_str().unwrap_or("none").to_owned();
            let (before, after) = (version(&change["before"]), version(&change["after"]));
            let moved = if before == after {
                format!("{before}, new build")
            } else {
                format!("{before} -> {after}")
            };
            let line = format!(
                "- {} {moved} ({{platforms}}): pulled in by {}",
                package["name"].as_str().unwrap_or("?"),
                introducers.join(", ")
            );
            let platform = package["platform"].as_str().unwrap_or("?");
            match explained.iter_mut().find(|(l, _)| *l == line) {
                Some((_, platforms)) => platforms.push(platform),
                None => explained.push((line, vec![platform])),
            }
        }
        for (line, platforms) in explained {
            lines.push(line.replace("{platforms}", &platforms.join(", ")));
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

    fn targets(ids: &[&str]) -> Vec<core::Target> {
        ids.iter()
            .map(|id| core::Target {
                id: id.to_string(),
                manager: id.split_once(':').unwrap().0.into(),
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
                    ..Default::default()
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
    fn new_targets_are_offered_by_manager_then_by_file() {
        let found = targets(&[
            "github-actions:.github/workflows/a.yml",
            "github-actions:.github/workflows/b.yml",
            "pixi:pixi.toml",
            "uv:pyproject.toml",
        ]);
        let mut asked = vec![];
        let choice = choose_targets(
            &found,
            &[],
            None,
            true,
            &mut scripted(&[false, true, false, true, false], &mut asked),
        )
        .unwrap();
        assert_eq!(
            asked,
            [
                "Select all 2 github-actions targets?",
                "Select github-actions:.github/workflows/a.yml?",
                "Select github-actions:.github/workflows/b.yml?",
                "Select pixi:pixi.toml?",
                "Select uv:pyproject.toml?",
            ]
        );
        assert_eq!(
            choice.added,
            ["github-actions:.github/workflows/a.yml", "pixi:pixi.toml"]
        );
        assert_eq!(choice.selected, choice.added);
        assert_eq!(
            choice.unselected,
            [
                "github-actions:.github/workflows/b.yml",
                "uv:pyproject.toml"
            ]
        );
    }

    #[test]
    fn a_rerun_offers_only_targets_not_yet_saved() {
        let found = targets(&["pixi:pixi.toml", "uv:pyproject.toml"]);
        let mut asked = vec![];
        let choice = choose_targets(
            &found,
            &["pixi:pixi.toml".into()],
            None,
            true,
            &mut scripted(&[true], &mut asked),
        )
        .unwrap();
        assert_eq!(asked, ["Select uv:pyproject.toml?"]);
        assert_eq!(choice.selected, ["pixi:pixi.toml", "uv:pyproject.toml"]);
        assert_eq!(choice.added, ["uv:pyproject.toml"]);
        assert!(choice.unselected.is_empty());
    }

    #[test]
    fn without_a_terminal_new_targets_are_listed_not_selected() {
        let found = targets(&["pixi:pixi.toml", "uv:pyproject.toml"]);
        let choice = choose_targets(&found, &["pixi:pixi.toml".into()], None, false, &mut |q| {
            panic!("unexpected prompt {q}")
        })
        .unwrap();
        assert_eq!(choice.selected, ["pixi:pixi.toml"]);
        assert!(choice.added.is_empty());
        assert_eq!(choice.unselected, ["uv:pyproject.toml"]);
    }

    #[test]
    fn a_manager_is_offered_once_wherever_its_targets_were_found() {
        let found = targets(&[
            "github-actions:a.yml",
            "pixi:pixi.toml",
            "github-actions:b.yml",
        ]);
        let mut asked = vec![];
        let choice = choose_targets(
            &found,
            &[],
            None,
            true,
            &mut scripted(&[true, false], &mut asked),
        )
        .unwrap();
        assert_eq!(
            asked,
            [
                "Select all 2 github-actions targets?",
                "Select pixi:pixi.toml?"
            ]
        );
        assert_eq!(
            choice.added,
            ["github-actions:a.yml", "github-actions:b.yml"]
        );
    }

    #[test]
    fn saved_targets_no_longer_found_are_kept_aside_and_requests_are_checked() {
        let found = targets(&["pixi:pixi.toml", "uv:pyproject.toml"]);
        let saved = ["pixi:gone/pixi.toml".into(), "pixi:pixi.toml".into()];
        let requested = ["uv:pyproject.toml".into(), "pixi:pixi.toml".into()];
        let choice = choose_targets(&found, &saved, Some(&requested), true, &mut |q| {
            panic!("unexpected prompt {q}")
        })
        .unwrap();
        assert_eq!(choice.stale, ["pixi:gone/pixi.toml"]);
        assert_eq!(choice.selected, ["pixi:pixi.toml", "uv:pyproject.toml"]);
        assert_eq!(choice.added, ["uv:pyproject.toml"]);
        let unknown = choose_targets(&found, &[], Some(&["npm:x".into()]), true, &mut |_| {
            Ok(true)
        });
        assert!(unknown.is_err());
    }

    #[test]
    fn declining_every_target_checks_and_saves_nothing() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("pyproject.toml"),
            "[project]\nname = \"p\"\nversion = \"0.1.0\"\n\n[tool.uv]\n",
        )
        .unwrap();
        let cli = Cli::try_parse_from([
            "depsmith",
            "--root",
            root.path().to_str().unwrap(),
            "--tool",
            "uv=depsmith-nonexistent-executable",
            "init",
        ])
        .unwrap();
        let config = core::config::settings(root.path(), &overrides(&cli).unwrap()).unwrap();
        let mut asked = vec![];
        let (report, status) = run_init(
            &cli,
            config,
            false,
            false,
            true,
            &mut scripted(&[false], &mut asked),
        )
        .unwrap();
        assert_eq!(asked, ["Select uv:pyproject.toml?"]);
        assert_eq!(status, 0);
        assert_eq!(report["targets"], json!([]));
        assert_eq!(report["tools"], json!([]));
        assert_eq!(report["config"]["unselected"], json!(["uv:pyproject.toml"]));
        assert!(!root.path().join("depsmith.toml").exists());
    }

    #[test]
    fn the_text_report_says_which_declared_dependencies_pulled_a_change_in() {
        let package = |version: &str, platform: &str| {
            json!({"ecosystem": "conda", "name": "libblas", "version": version,
                "artifact": "a", "platform": platform})
        };
        let report = json!({"schema_version": 1, "proposal": {
        "changes": [], "suggestions": [], "unresolved": [], "failures": [],
        "scans": [], "validation": [],
        "dependencies": [
            {"before": package("1.0", "linux-64"), "after": package("2.0", "linux-64"),
                "introducers": ["numpy", "scipy"], "paths": [["numpy", "libblas"]]},
            {"before": package("1.0", "win-64"), "after": package("2.0", "win-64"),
                "introducers": ["numpy", "scipy"], "paths": [["numpy", "libblas"]]},
            {"before": package("1.0", "osx-64"), "after": package("1.1", "osx-64")},
        ]}});
        let text = render(&report, false);
        assert_eq!(
            text.matches("- libblas 1.0 -> 2.0 (linux-64, win-64): pulled in by numpy, scipy")
                .count(),
            1,
            "{text}"
        );
        assert!(!text.contains("1.1"), "{text}");
        let rebuilt = json!({"schema_version": 1, "proposal": {
            "changes": [], "suggestions": [], "unresolved": [], "failures": [],
            "scans": [], "validation": [],
            "dependencies": [{"before": package("2.0", "linux-64"),
                "after": package("2.0", "linux-64"), "introducers": ["numpy"],
                "paths": [["numpy", "libblas"]]}]}});
        let text = render(&rebuilt, false);
        assert!(
            text.contains("- libblas 2.0, new build (linux-64): pulled in by numpy"),
            "{text}"
        );
    }
}

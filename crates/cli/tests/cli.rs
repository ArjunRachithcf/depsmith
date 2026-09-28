use std::{fs, process::Command};
use tempfile::tempdir;
#[test]
fn json_discovery_is_machine_readable() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("pixi.toml"), "[workspace]\nname='demo'\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_depsmith"))
        .args([
            "discover",
            "--root",
            root.path().to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["targets"][0]["id"], "pixi:pixi.toml");
}
#[test]
fn noninteractive_ambiguous_selection_returns_configuration_error() {
    let root = tempdir().unwrap();
    fs::write(root.path().join("pixi.toml"), "[workspace]\nname='demo'\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_depsmith"))
        .args([
            "check",
            "--root",
            root.path().to_str().unwrap(),
            "--json",
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(result["error"].as_str().unwrap().contains("target"));
}

#[cfg(unix)]
#[test]
fn backend_cannot_inherit_repository_overrides_but_keeps_git_auth() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempdir().unwrap();
    let tools = tempdir().unwrap();
    fs::write(root.path().join("pixi.toml"), "[workspace]\nname='demo'\n").unwrap();
    let backend = tools.path().join("pixi");
    fs::write(&backend, r#"#!/bin/sh
test -z "$GIT_DIR$GIT_WORK_TREE$GIT_INDEX_FILE$GIT_COMMON_DIR$GIT_OBJECT_DIRECTORY$GIT_ALTERNATE_OBJECT_DIRECTORIES$GIT_CONFIG_PARAMETERS$GIT_CONFIG_COUNT$GIT_SHALLOW_FILE" || exit 77
test "$GIT_ASKPASS" = fixture-auth || exit 78
if [ "$1" = update ]; then printf 'version: 7\npackages: []\n' > pixi.lock; fi
"#).unwrap();
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_depsmith"));
    command.args([
        "check",
        "--root",
        root.path().to_str().unwrap(),
        "--target",
        "pixi:pixi.toml",
        "--pixi",
        backend.to_str().unwrap(),
        "--json",
    ]);
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_SHALLOW_FILE",
    ] {
        command.env(key, "/outside");
    }
    command
        .env("GIT_CONFIG_PARAMETERS", "'core.worktree=/outside'")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.worktree")
        .env("GIT_CONFIG_VALUE_0", "/outside")
        .env("GIT_ASKPASS", "fixture-auth");
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!root.path().join("pixi.lock").exists());
}

#[test]
fn doctor_explains_prerequisites_in_plain_text() {
    let output = Command::new(env!("CARGO_BIN_EXE_depsmith"))
        .args([
            "doctor",
            "--pixi",
            "depsmith-nonexistent-executable",
            "--grype",
            "depsmith-nonexistent-executable",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.trim_start().starts_with('{'), "{text}");
    assert!(text.contains("pixi: unavailable"), "{text}");
    assert!(text.contains("tested: 0.80.0"), "{text}");
    assert!(
        text.contains("cooldown: unsupported (a common cooldown is not implemented"),
        "{text}"
    );
    assert!(
        text.contains("install_validation: not-applicable"),
        "{text}"
    );
}

#[test]
fn text_report_lists_validation_notes() {
    let root = tempdir().unwrap();
    let workflows = root.path().join(".github/workflows");
    fs::create_dir_all(&workflows).unwrap();
    fs::write(
        workflows.join("ci.yml"),
        "on: push\njobs:\n  b:\n    runs-on: x\n    steps:\n      - uses: ./local\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_depsmith"))
        .args([
            "check",
            "--root",
            root.path().to_str().unwrap(),
            "--target",
            "github-actions:.github/workflows/ci.yml",
            "--non-interactive",
            "--install",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains(
            "- Validation: github-actions:.github/workflows/ci.yml: --install not applicable"
        ),
        "{text}"
    );
}

#[cfg(unix)]
#[test]
fn interrupt_cancels_the_backend_process_tree() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempdir().unwrap();
    let tools = tempdir().unwrap();
    fs::write(root.path().join("pixi.toml"), "[workspace]\nname='demo'\n").unwrap();
    let pidfile = tools.path().join("grandchild.pid");
    let pixi = tools.path().join("pixi");
    fs::write(
        &pixi,
        format!(
            "#!/bin/sh\nsleep 60 &\necho $! > '{}'\nwait\n",
            pidfile.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&pixi, fs::Permissions::from_mode(0o755)).unwrap();
    let mut cli = Command::new(env!("CARGO_BIN_EXE_depsmith"))
        .args([
            "check",
            "--root",
            root.path().to_str().unwrap(),
            "--target",
            "pixi:pixi.toml",
            "--non-interactive",
            "--json",
            "--pixi",
            pixi.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let waited = std::time::Instant::now();
    while !pidfile.exists() || fs::read_to_string(&pidfile).unwrap().trim().is_empty() {
        assert!(waited.elapsed().as_secs() < 20, "backend never started");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let grandchild = fs::read_to_string(&pidfile).unwrap().trim().to_owned();
    Command::new("kill")
        .args(["-INT", &cli.id().to_string()])
        .status()
        .unwrap();
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = cli.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed().as_secs() < 10,
            "CLI ignored the interrupt"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let alive = |pid: &str| {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while alive(&grandchild) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let survived = alive(&grandchild);
    if survived {
        let _ = Command::new("kill").args(["-KILL", &grandchild]).status();
    }
    assert!(!survived, "backend grandchild survived the interrupt");
    assert_eq!(status.code(), Some(130));
}

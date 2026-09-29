#[cfg(unix)]
#[test]
fn progress_environment_uses_the_boolean_value_accepted_by_pixi() {
    let result = depsmith_core::process::run(
        "sh",
        &["-c".into(), "test \"$PIXI_NO_PROGRESS\" = true".into()],
        std::path::Path::new("."),
        5,
    );
    assert!(result.is_ok(), "{result:?}");
}

#[cfg(unix)]
fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[cfg(unix)]
#[test]
fn timeout_kills_the_whole_process_tree() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("grandchild.pid");
    let script = format!("sleep 30 & echo $! > '{}'; wait", pidfile.display());
    let error =
        depsmith_core::process::run("sh", &["-c".into(), script], dir.path(), 1).unwrap_err();
    assert!(error.to_string().contains("timeout"), "{error}");
    let pid = std::fs::read_to_string(&pidfile).unwrap().trim().to_owned();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while alive(&pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let survived = alive(&pid);
    if survived {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid])
            .status();
    }
    assert!(!survived, "grandchild {pid} outlived the timeout");
}

#[cfg(unix)]
#[test]
fn failures_report_a_redacted_output_tail() {
    let script = r#"
i=1; while [ $i -le 60 ]; do echo "noise line $i" >&2; i=$((i+1)); done
echo "fetching https://alice:hunter2@example.org/simple/?token=abc123&page=2" >&2
echo "Authorization: Bearer eyJhbGciOi.payload.signature" >&2
echo "using ghp_0123456789abcdefghijklmnopqrstuvwxyzAB" >&2
echo "api key s3cr3t-value-from-env was rejected" >&2
echo "ruff 0.16.9 is excluded because the package is uploaded after the cutoff date" >&2
exit 1
"#;
    let error = depsmith_core::process::run_env(
        "sh",
        &["-c".into(), script.into()],
        std::path::Path::new("."),
        10,
        &[("DEPSMITH_TEST_TOKEN".into(), "s3cr3t-value-from-env".into())],
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("ruff 0.16.9 is excluded because"), "{error}");
    for secret in [
        "hunter2",
        "abc123",
        "eyJhbGciOi",
        "ghp_0123456789",
        "s3cr3t-value",
    ] {
        assert!(!error.contains(secret), "leaked {secret}: {error}");
    }
    assert!(
        error.contains("https://***@example.org/simple/?token=***&page=2"),
        "{error}"
    );
    assert!(
        !error.contains("noise line 1\n"),
        "output must be limited to the tail: {error}"
    );
}

/// A freshly written executable can be briefly busy (ETXTBSY) while any
/// process still holds a write handle, e.g. a concurrently forked child that
/// inherited it. Starting the tool must wait that out rather than fail.
#[cfg(unix)]
#[test]
fn a_briefly_busy_executable_still_starts() {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("tool");
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(&script)
        .unwrap();
    writer.write_all(b"#!/bin/sh\necho started\n").unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        drop(writer);
    });
    let result = depsmith_core::process::run(script.to_str().unwrap(), &[], dir.path(), 5);
    release.join().unwrap();
    assert_eq!(result.unwrap().trim(), "started");
}

//! End-to-end tests of the real `depsmith` executable on every host OS.
//!
//! This test executable doubles as the backend: with `DEPSMITH_E2E_FAKE` set it
//! behaves like the parts of Pixi depsmith uses, so no shell scripts are needed.
use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

const FAKE: &str = "DEPSMITH_E2E_FAKE";
/// Original manifest the fake edits mid-resolution (a concurrent user edit).
const EDIT: &str = "DEPSMITH_E2E_EDIT";
/// File the fake writes its long-running child's PID to before hanging.
const HANG: &str = "DEPSMITH_E2E_HANG";
const LOCK: &str = "version: 6\npackages: []\n";

fn main() {
    if let Ok(mode) = env::var(FAKE) {
        fake(&mode);
        return;
    }
    let scenarios: [(&str, fn()); 3] = [
        ("check, apply and recheck", check_apply_recheck),
        (
            "a concurrent edit makes apply refuse",
            concurrent_edit_is_refused,
        ),
        (
            "timeout kills the backend's process tree",
            timeout_kills_process_tree,
        ),
    ];
    let mut failed = 0;
    for (name, scenario) in scenarios {
        let outcome = std::panic::catch_unwind(scenario);
        println!(
            "{} ... {name}",
            if outcome.is_ok() { "ok" } else { "FAILED" }
        );
        failed += usize::from(outcome.is_err());
    }
    if failed > 0 {
        eprintln!("{failed} end-to-end scenario(s) failed");
        std::process::exit(1);
    }
}

/// `pixi update|lock|... --manifest-path M`, run in depsmith's staged copy.
fn fake(mode: &str) {
    if mode == "sleep" {
        thread::sleep(Duration::from_secs(60));
        return;
    }
    let args: Vec<String> = env::args().skip(1).collect();
    let manifest = args
        .iter()
        .position(|a| a == "--manifest-path")
        .map(|i| PathBuf::from(&args[i + 1]))
        .expect("--manifest-path");
    let lock = manifest.with_file_name("pixi.lock");
    match args[0].as_str() {
        "update" => {
            if let Ok(pidfile) = env::var(HANG) {
                let mut child = Command::new(env::current_exe().unwrap())
                    .env(FAKE, "sleep")
                    .spawn()
                    .unwrap();
                fs::write(pidfile, child.id().to_string()).unwrap();
                // Hang on the grandchild until depsmith's timeout kills the tree.
                let _ = child.wait();
            }
            if let Ok(original) = env::var(EDIT) {
                let text = fs::read_to_string(&original).unwrap();
                fs::write(&original, format!("{text}# edited during resolution\n")).unwrap();
            }
            fs::write(lock, LOCK).unwrap();
        }
        "lock" => assert_eq!(fs::read_to_string(lock).unwrap(), LOCK),
        other => panic!("unexpected pixi command {other}"),
    }
}

struct Project {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn project() -> Project {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::write(
        root.join("pixi.toml"),
        "[workspace]\nname = 'demo'\nchannels = ['conda-forge']\nplatforms = ['linux-64']\n",
    )
    .unwrap();
    Project { _dir: dir, root }
}

fn depsmith(project: &Project, args: &[&str], env: &[(&str, &str)]) -> (i32, serde_json::Value) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_depsmith"));
    command
        .args(args)
        .args([
            "--root",
            project.root.to_str().unwrap(),
            "--target",
            "pixi:pixi.toml",
        ])
        .args(["--pixi", env::current_exe().unwrap().to_str().unwrap()])
        .args(["--non-interactive", "--json"])
        .env(FAKE, "pixi");
    for (key, value) in env {
        command.env(key, value);
    }
    let Output {
        status,
        stdout,
        stderr,
    } = command.output().unwrap();
    let report = serde_json::from_slice(&stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        )
    });
    (status.code().expect("exit code"), report)
}

fn check_apply_recheck() {
    let project = project();
    let (code, report) = depsmith(&project, &["check"], &[]);
    assert_eq!(code, 1, "pending changes: {report}");
    assert!(
        !project.root.join("pixi.lock").exists(),
        "check must not write"
    );
    let (code, report) = depsmith(&project, &["update", "--apply", "--yes"], &[]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        fs::read_to_string(project.root.join("pixi.lock")).unwrap(),
        LOCK
    );
    let (code, report) = depsmith(&project, &["check"], &[]);
    assert_eq!(code, 0, "no changes after apply: {report}");
}

fn concurrent_edit_is_refused() {
    let project = project();
    let manifest = project.root.join("pixi.toml");
    let (code, report) = depsmith(
        &project,
        &["update", "--apply", "--yes"],
        &[(EDIT, manifest.to_str().unwrap())],
    );
    assert_ne!(code, 0, "{report}");
    assert!(
        report["error"]
            .as_str()
            .is_some_and(|e| e.contains("proposal is stale")),
        "{report}"
    );
    assert!(
        !project.root.join("pixi.lock").exists(),
        "stale candidate applied"
    );
    assert!(fs::read_to_string(&manifest)
        .unwrap()
        .ends_with("# edited during resolution\n"));
}

fn timeout_kills_process_tree() {
    let project = project();
    let pidfile = project
        .root
        .join("..")
        .join(format!("e2e-{}.pid", std::process::id()));
    let started = Instant::now();
    let (code, report) = depsmith(
        &project,
        &["check", "--timeout-seconds", "2"],
        &[(HANG, pidfile.to_str().unwrap())],
    );
    assert_eq!(code, 3, "{report}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "timeout not enforced"
    );
    let pid = fs::read_to_string(&pidfile).unwrap();
    let _ = fs::remove_file(&pidfile);
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid.trim()) {
        assert!(
            Instant::now() < deadline,
            "grandchild {pid} survived the timeout"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn alive(pid: &str) -> bool {
    if cfg!(windows) {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    } else {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    }
}

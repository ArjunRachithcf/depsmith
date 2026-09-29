//! Running package managers and scanners without a shell: timeouts,
//! cancellation of whole process trees, and diagnostics with credentials
//! redacted.
use crate::{Error, Result};
use regex::Regex;
use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

static CANCELLED: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Request cancellation: running backends are killed with their process
/// trees and later runs fail immediately.
pub fn cancel() {
    CANCELLED.store(true, Ordering::SeqCst);
}

/// Whether [`cancel`] has been requested.
pub fn cancelled() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}

/// For executables only (never a library host such as Python): make SIGINT and
/// SIGTERM cancel running backends. With none running, the default action is
/// restored and the signal re-raised, so the process terminates as usual.
#[cfg(unix)]
pub fn install_cancellation_handlers() {
    extern "C" fn handle(signal: libc::c_int) {
        // Only async-signal-safe operations: atomics, signal and raise.
        if RUNNING.load(Ordering::SeqCst) == 0 {
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
        } else {
            CANCELLED.store(true, Ordering::SeqCst);
        }
    }
    for signal in [libc::SIGINT, libc::SIGTERM] {
        unsafe {
            libc::signal(
                signal,
                handle as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
    }
}

/// On Windows, console Ctrl-C already reaches backends in the same console.
#[cfg(not(unix))]
pub fn install_cancellation_handlers() {}

/// Kill the backend and everything it started. Unix backends lead their own
/// process group; Windows uses `taskkill /T`. Descendants that deliberately
/// leave the group (e.g. via `setsid`) are not tracked.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn secret_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "CREDENTIAL",
        "AUTH",
    ]
    .iter()
    .any(|marker| name.contains(marker))
}

/// Remove credentials from backend output: values of secret-named environment
/// variables, URL userinfo, token-like parameters, authorization headers and
/// GitHub token formats.
pub(crate) fn redact(text: &str, env: &[(String, String)]) -> String {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            (r"(?i)\b([a-z][a-z0-9+.-]*://)[^/\s@]+@", "${1}***@"),
            (r"(?i)\b((?:access_|auth_|api_|private_|client_)?(?:token|key|secret|password|passwd|pwd|signature|sig|apikey|api-key)=)[^&\s\x22']+", "${1}***"),
            (r"(?i)\b(authorization:\s*(?:bearer|basic|token)?\s*)\S+", "${1}***"),
            (r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]+", "${1}***"),
            (r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})", "***"),
        ]
        .into_iter()
        .map(|(pattern, replacement)| (Regex::new(pattern).unwrap(), replacement))
        .collect()
    });
    let mut text = text.to_owned();
    let values = std::env::vars()
        .chain(env.iter().cloned())
        .filter(|(name, value)| secret_name(name) && value.len() >= 6);
    for (_, value) in values {
        text = text.replace(&value, "***");
    }
    for (pattern, replacement) in patterns {
        text = pattern.replace_all(&text, *replacement).into_owned();
    }
    text
}

/// The last lines of captured stderr, redacted, for failure diagnostics.
fn tail(mut errors: std::fs::File, env: &[(String, String)]) -> String {
    const LINES: usize = 40;
    const BYTES: u64 = 64 * 1024;
    let length = errors.metadata().map_or(0, |m| m.len());
    let mut bytes = vec![];
    if errors
        .seek(SeekFrom::Start(length.saturating_sub(BYTES)))
        .is_err()
        || errors.read_to_end(&mut bytes).is_err()
    {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let kept = lines[lines.len().saturating_sub(LINES)..].join("\n");
    redact(&kept, env)
}

fn diagnostics(errors: std::fs::File, env: &[(String, String)]) -> String {
    let tail = tail(errors, env);
    if tail.trim().is_empty() {
        String::new()
    } else {
        format!("\nlast output lines (credentials redacted):\n{tail}")
    }
}

/// Execute without a shell. Temporary files avoid pipe deadlocks for verbose tools.
/// Failures report a bounded, redacted stderr tail; stdout is never echoed.
pub fn run(program: &str, args: &[String], cwd: &Path, timeout: u64) -> Result<String> {
    run_env(program, args, cwd, timeout, &[])
}

/// Like [`run`], with extra environment variables whose values are also
/// redacted from diagnostics.
pub fn run_env(
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout: u64,
    env: &[(String, String)],
) -> Result<String> {
    let mut command = Command::new(program);
    command.args(args).envs(env.iter().cloned());
    run_with_env(command, program, cwd, timeout, env)
}

pub(crate) fn run_command(
    command: Command,
    program: &str,
    cwd: &Path,
    timeout: u64,
) -> Result<String> {
    run_with_env(command, program, cwd, timeout, &[])
}

fn run_with_env(
    mut command: Command,
    program: &str,
    cwd: &Path,
    timeout: u64,
    env: &[(String, String)],
) -> Result<String> {
    // Keep authentication (e.g. GIT_ASKPASS/GIT_SSH_COMMAND), but do not let a
    // caller's repository context redirect build-hook Git operations out of
    // the stage. Git config injection can set core.worktree too.
    for key in [
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_OBJECT_DIRECTORY",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_GRAFT_FILE",
        "GIT_INDEX_FILE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_REPLACE_REF_BASE",
        "GIT_PREFIX",
        "GIT_SHALLOW_FILE",
        "GIT_COMMON_DIR",
    ] {
        command.env_remove(key);
    }
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("GIT_CONFIG_KEY_") || name.starts_with("GIT_CONFIG_VALUE_") {
            command.env_remove(key);
        }
    }
    if timeout == 0 {
        return Err(Error::Invalid("timeout must be positive".into()));
    }
    if cancelled() {
        return Err(Error::Operation(format!(
            "cancelled before starting {program}"
        )));
    }
    let output = tempfile::tempfile()?;
    let errors = tempfile::tempfile()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    RUNNING.fetch_add(1, Ordering::SeqCst);
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(errors.try_clone()?)
        .env("NO_COLOR", "1")
        .env("PIXI_NO_PROGRESS", "true");
    // A just-written executable is busy (ETXTBSY) while any process still holds
    // a write handle, such as a child forked concurrently that inherited it
    // until its own exec. That clears within moments; retry for up to ~2 s.
    let mut spawned = command.spawn();
    for _ in 0..40 {
        match &spawned {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(50));
                spawned = command.spawn();
            }
            _ => break,
        }
    }
    let result = match spawned {
        Ok(mut child) => wait(&mut child, program, timeout, output, errors, env),
        Err(e) => Err(Error::Operation(format!("cannot start {program}: {e}"))),
    };
    RUNNING.fetch_sub(1, Ordering::SeqCst);
    result
}

fn wait(
    child: &mut Child,
    program: &str,
    timeout: u64,
    mut output: std::fs::File,
    errors: std::fs::File,
    env: &[(String, String)],
) -> Result<String> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(Error::Operation(format!(
                    "{program} failed with {status}{}",
                    diagnostics(errors, env)
                )));
            }
            output.seek(SeekFrom::Start(0))?;
            let mut text = String::new();
            output.take(32 * 1024 * 1024).read_to_string(&mut text)?;
            return Ok(text);
        }
        if cancelled() {
            kill_tree(child);
            return Err(Error::Operation(format!("{program} was cancelled")));
        }
        if start.elapsed() >= Duration::from_secs(timeout) {
            kill_tree(child);
            return Err(Error::Operation(format!(
                "{program} exceeded {timeout}s timeout{}",
                diagnostics(errors, env)
            )));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

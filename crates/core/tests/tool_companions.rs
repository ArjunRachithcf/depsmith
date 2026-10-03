//! `depsmith init` installs a missing companion a setup step needs (uv for a
//! Python tool) before the tool itself, asking for it with the reason. One
//! test: the cache location comes from the process-wide `DEPSMITH_TOOLS_DIR`.
//! Unix-only: the tools are shell scripts.
#![cfg(unix)]
use depsmith_core::{
    adapter::{Adapter, AdapterSpec, Candidate, ToolSpec},
    provision::{self, Archive, Setup, ToolDownload},
    Engine, Target, UpdateOptions,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
};

/// A uv that builds the venv layout and the Python tool's entry point.
const UV: &[u8] = b"#!/bin/sh\nset -e\nPATH=/usr/bin:/bin\ncase \"$1\" in\n  venv) for a; do last=$a; done; mkdir -p \"$last/bin\";;\n  pip) for a; do case \"$a\" in */venv) venv=$a;; esac; done; printf '#!/bin/sh\\necho conda-lock 4.0.2\\n' > \"$venv/bin/conda-lock\"; chmod 755 \"$venv/bin/conda-lock\";;\nesac\n";

/// Serve the uv stand-in at `/uv` on 127.0.0.1.
fn serve() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let mut stream = &stream;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                UV.len()
            );
            let _ = stream.write_all(UV);
        }
    });
    base
}

fn tool(name: &str, version: &str, downloads: Vec<ToolDownload>) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        default: name.into(),
        tested_versions: vec![version.into()],
        downloads,
    }
}

/// An adapter for `lock.toml` files using a Python tool, plus one that only
/// declares uv (so the engine knows how to install it).
struct Uses(ToolSpec);
impl Adapter for Uses {
    fn spec(&self) -> AdapterSpec {
        let mut spec = AdapterSpec::new(&format!("uses-{}", self.0.name), &["lock.toml"]);
        spec.tools = vec![self.0.clone()];
        spec
    }
    fn detects(&self, _: &Path, _: &str) -> bool {
        self.0.name == "conda-lock"
    }
    fn prepare(&self, _: &Path, _: &Target, _: &UpdateOptions) -> depsmith_core::Result<Candidate> {
        Ok(Candidate::default())
    }
}

#[test]
fn a_missing_companion_is_offered_and_installed_first() {
    let cache = tempfile::tempdir().unwrap();
    std::env::set_var("DEPSMITH_TOOLS_DIR", cache.path());
    // Neither uv nor conda-lock may be found on PATH.
    let empty = tempfile::tempdir().unwrap();
    std::env::set_var("PATH", empty.path());
    let base = serve();
    let uv = tool(
        "uv",
        "0.12.15",
        vec![ToolDownload {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            url: format!("{base}/uv"),
            sha256: format!("{:x}", Sha256::digest(UV)),
            archive: Archive::Binary,
            executable: "uv".into(),
            ..Default::default()
        }],
    );
    let conda_lock = tool(
        "conda-lock",
        "4.0.2",
        provision::pinned("conda-lock")
            .into_iter()
            .filter(|d| d.os == std::env::consts::OS)
            .collect(),
    );
    assert!(matches!(
        provision::host_download(&conda_lock).and_then(|d| d.setup.clone()),
        Some(Setup::UvVenv { .. })
    ));
    let engine = Engine::new(vec![Box::new(Uses(conda_lock)), Box::new(Uses(uv))]);
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("lock.toml"), "").unwrap();

    let mut asked = vec![];
    let report = engine
        .init(
            root.path(),
            &[],
            &UpdateOptions::default(),
            &mut |tool, reason| {
                asked.push((tool.name.clone(), reason.to_owned()));
                Ok(true)
            },
        )
        .unwrap();
    assert_eq!(
        asked,
        [
            ("conda-lock".to_owned(), "not installed".to_owned()),
            ("uv".to_owned(), "needed to install conda-lock".to_owned()),
        ]
    );
    assert_eq!(
        report["installed"],
        serde_json::json!(["uv", "conda-lock"]),
        "{report}"
    );
    assert_eq!(report["missing"], serde_json::json!([]), "{report}");
    assert_eq!(report["tools"][0]["source"], "downloaded");
}

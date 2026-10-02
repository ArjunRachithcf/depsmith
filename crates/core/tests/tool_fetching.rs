//! Missing tools at the engine: `init` reports the tools the targets use and
//! installs a missing one only with consent, once; `prepare` never blocks on
//! a missing tool and picks up the installed one from the tool cache. One test, because the cache
//! location comes from the process-wide `DEPSMITH_TOOLS_DIR`. Unix-only: the
//! fetched tool is a shell script.
#![cfg(unix)]
use depsmith_core::{
    adapter::{Adapter, AdapterSpec, Candidate, ToolSpec},
    provision::{Archive, ToolDownload},
    Engine, Target, UpdateOptions,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    sync::{Arc, Mutex},
};

const TOOL: &str = "depsmith-test-fetched-tool";
const BROKEN: &str = "depsmith-test-unpublished-tool";

/// Serve `body` at `/tool` on 127.0.0.1 (other paths get 404), counting
/// requests.
fn serve(body: Vec<u8>) -> (String, Arc<Mutex<usize>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(Mutex::new(0));
    let seen = count.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap_or_default();
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            *seen.lock().unwrap() += 1;
            let found = request.split_whitespace().nth(1) == Some("/tool");
            let (status, body) = if found {
                ("200 OK", body.as_slice())
            } else {
                ("404 Not Found", &[][..])
            };
            let mut stream = &stream;
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(body);
        }
    });
    (base, count)
}

/// An adapter for `tool.toml` files using two tools: TOOL, whose preparation
/// runs and records its output, and BROKEN, whose download does not exist.
struct Fetching {
    downloads: Vec<ToolDownload>,
}

impl Adapter for Fetching {
    fn spec(&self) -> AdapterSpec {
        let mut spec = AdapterSpec::new("fetching", &["tool.toml"]);
        spec.tools = [TOOL, BROKEN]
            .iter()
            .zip(&self.downloads)
            .map(|(name, download)| ToolSpec {
                name: (*name).into(),
                default: (*name).into(),
                tested_versions: vec!["1.2.3".into()],
                downloads: vec![download.clone()],
            })
            .collect();
        spec
    }
    fn detects(&self, _: &Path, _: &str) -> bool {
        true
    }
    fn prepare(
        &self,
        stage: &Path,
        _: &Target,
        options: &UpdateOptions,
    ) -> depsmith_core::Result<Candidate> {
        let output = depsmith_core::process::run(&options.tool(TOOL), &[], stage, 30)?;
        Ok(Candidate {
            validation: vec![output.trim().to_owned()],
            ..Default::default()
        })
    }
}

type Asked = Vec<(String, String)>;

/// Run init, answering `answer` and recording (tool, reason) per question.
fn init(
    engine: &Engine,
    root: &Path,
    selected: &[String],
    options: &UpdateOptions,
    answer: bool,
) -> (serde_json::Value, Asked) {
    let mut asked = vec![];
    let report = engine
        .init(root, selected, options, &mut |tool, reason| {
            asked.push((tool.name.clone(), reason.to_owned()));
            Ok(answer)
        })
        .unwrap();
    (report, asked)
}

fn names(value: &serde_json::Value) -> Vec<&str> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().or_else(|| v["tool"].as_str()).unwrap())
        .collect()
}

#[test]
fn init_installs_missing_used_tools_only_with_consent() {
    let cache = tempfile::tempdir().unwrap();
    std::env::set_var("DEPSMITH_TOOLS_DIR", cache.path());
    let program = b"#!/bin/sh\necho 'fetched tool ran'\n".to_vec();
    let (base, requests) = serve(program.clone());
    let download = |name: &str, path: &str| ToolDownload {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        url: format!("{base}{path}"),
        sha256: format!("{:x}", Sha256::digest(&program)),
        archive: Archive::Binary,
        executable: name.into(),
    };
    let engine = Engine::new(vec![Box::new(Fetching {
        downloads: vec![download(TOOL, "/tool"), download(BROKEN, "/broken")],
    })]);
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("tool.toml"), "").unwrap();
    let targets = ["fetching:tool.toml".to_owned()];
    let none = UpdateOptions::default();

    let found = engine.discover(root.path()).unwrap();
    let missing = engine.missing_tools(root.path(), &found, &none);
    assert_eq!(
        missing.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        [TOOL, BROKEN]
    );

    // A missing tool fails its target when it runs, not the whole run.
    let before = engine.prepare(root.path(), &targets, none.clone()).unwrap();
    assert_eq!(before.failures.len(), 1, "{:?}", before.failures);

    // Only the tools the targets use are checked; the scanner only when
    // scanning, and an unknown target is an error.
    let (declined, asked) = init(&engine, root.path(), &[], &none, false);
    assert_eq!(
        asked,
        [TOOL, BROKEN].map(|t| (t.to_owned(), "not installed".to_owned()))
    );
    assert_eq!(names(&declined["tools"]), [TOOL, BROKEN]);
    assert_eq!(names(&declined["missing"]), [TOOL, BROKEN]);
    assert_eq!(declined["installed"], serde_json::json!([]));
    assert_eq!(declined["tools"][0]["used_by"], serde_json::json!(targets));
    assert_eq!(*requests.lock().unwrap(), 0);
    let scanning = UpdateOptions {
        scan: true,
        ..none.clone()
    };
    let (with_scanner, _) = init(&engine, root.path(), &targets, &scanning, false);
    assert_eq!(names(&with_scanner["tools"]), [TOOL, BROKEN, "grype"]);
    assert!(engine
        .init(
            root.path(),
            &["fetching:absent.toml".into()],
            &none,
            &mut |_, _| Ok(true)
        )
        .is_err());

    // One failed install is reported and does not stop the others.
    let (accepted, _) = init(&engine, root.path(), &[], &none, true);
    assert_eq!(names(&accepted["installed"]), [TOOL]);
    assert_eq!(names(&accepted["failed"]), [BROKEN]);
    assert!(
        accepted["failed"][0]["error"]
            .as_str()
            .unwrap()
            .contains("404"),
        "{}",
        accepted["failed"][0]
    );
    assert_eq!(names(&accepted["missing"]), [BROKEN]);
    assert_eq!(*requests.lock().unwrap(), 2);

    // Installed once: later runs use the cache without asking or fetching it.
    let (again, asked) = init(&engine, root.path(), &[], &none, false);
    assert_eq!(asked, [(BROKEN.to_owned(), "not installed".to_owned())]);
    assert_eq!(again["tools"][0]["source"], "downloaded");
    let fetched = engine.prepare(root.path(), &targets, none.clone()).unwrap();
    assert!(fetched.failures.is_empty(), "{:?}", fetched.failures);
    assert!(fetched.validation.iter().any(|v| v == "fetched tool ran"));
    assert_eq!(*requests.lock().unwrap(), 2);

    // A changed install is not run; init says why and downloads afresh.
    let installed = again["tools"][0]["program"].as_str().unwrap().to_owned();
    fs::write(&installed, "#!/bin/sh\necho tampered\n").unwrap();
    let refused = engine.prepare(root.path(), &targets, none.clone()).unwrap();
    assert!(
        refused.validation.iter().any(|v| {
            v.contains("changed since depsmith installed it") && v.contains("depsmith init")
        }),
        "{:?}",
        refused.validation
    );
    assert!(!refused.validation.iter().any(|v| v == "tampered"));
    let (restored, asked) = init(&engine, root.path(), &targets, &none, true);
    assert!(
        asked[0].0 == TOOL && asked[0].1.contains("changed since depsmith installed it"),
        "{asked:?}"
    );
    assert_eq!(names(&restored["installed"]), [TOOL]);
    let fetched = engine.prepare(root.path(), &targets, none).unwrap();
    assert!(fetched.validation.iter().any(|v| v == "fetched tool ran"));
}

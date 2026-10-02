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

/// Serve `body` at `/tool` on 127.0.0.1, counting requests.
fn serve(body: Vec<u8>) -> (String, Arc<Mutex<usize>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(Mutex::new(0));
    let seen = count.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            *seen.lock().unwrap() += 1;
            let mut stream = &stream;
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    (base, count)
}

/// An adapter for `tool.toml` files whose preparation runs its tool and
/// records the output as validation.
struct Fetching {
    download: ToolDownload,
}

impl Adapter for Fetching {
    fn spec(&self) -> AdapterSpec {
        let mut spec = AdapterSpec::new("fetching", &["tool.toml"]);
        spec.tools = vec![ToolSpec {
            name: TOOL.into(),
            default: TOOL.into(),
            tested_versions: vec!["1.2.3".into()],
            downloads: vec![self.download.clone()],
        }];
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

#[test]
fn init_installs_missing_used_tools_only_with_consent() {
    let cache = tempfile::tempdir().unwrap();
    std::env::set_var("DEPSMITH_TOOLS_DIR", cache.path());
    let program = b"#!/bin/sh\necho 'fetched tool ran'\n".to_vec();
    let (base, requests) = serve(program.clone());
    let engine = Engine::new(vec![Box::new(Fetching {
        download: ToolDownload {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            url: format!("{base}/tool"),
            sha256: format!("{:x}", Sha256::digest(&program)),
            archive: Archive::Binary,
            executable: TOOL.into(),
        },
    })]);
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("tool.toml"), "").unwrap();
    let targets = ["fetching:tool.toml".to_owned()];

    let found = targets_of(&engine, root.path());
    let missing = engine.missing_tools(root.path(), &found, &UpdateOptions::default());
    assert_eq!(
        missing.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        [TOOL]
    );

    // A missing tool fails its target when it runs, not the whole run.
    let before = engine
        .prepare(root.path(), &targets, UpdateOptions::default())
        .unwrap();
    assert_eq!(before.failures.len(), 1, "{:?}", before.failures);

    let mut asked = vec![];
    let declined = engine
        .init(root.path(), &UpdateOptions::default(), &mut |t| {
            asked.push(t.name.clone());
            false
        })
        .unwrap();
    assert_eq!(asked, [TOOL]);
    assert_eq!(declined["installed"], serde_json::json!([]));
    assert_eq!(declined["missing"], serde_json::json!([TOOL]));
    assert_eq!(declined["tools"][0]["used_by"], serde_json::json!(targets));
    assert_eq!(declined["tools"][0]["source"], "missing");
    assert_eq!(*requests.lock().unwrap(), 0);

    let accepted = engine
        .init(root.path(), &UpdateOptions::default(), &mut |_| true)
        .unwrap();
    assert_eq!(accepted["installed"], serde_json::json!([TOOL]));
    assert_eq!(accepted["missing"], serde_json::json!([]));
    assert_eq!(*requests.lock().unwrap(), 1);

    // Installed once: later runs use the cache without asking or fetching.
    let again = engine
        .init(root.path(), &UpdateOptions::default(), &mut |_| {
            panic!("nothing is missing")
        })
        .unwrap();
    assert_eq!(again["tools"][0]["source"], "downloaded");
    let fetched = engine
        .prepare(root.path(), &targets, UpdateOptions::default())
        .unwrap();
    assert!(fetched.failures.is_empty(), "{:?}", fetched.failures);
    assert!(
        fetched.validation.iter().any(|v| v == "fetched tool ran"),
        "{:?}",
        fetched.validation
    );
    assert_eq!(*requests.lock().unwrap(), 1);

    // A changed install is not run; init asks again and downloads afresh.
    let installed = again["tools"][0]["program"].as_str().unwrap().to_owned();
    fs::write(&installed, "#!/bin/sh\necho tampered\n").unwrap();
    let refused = engine
        .prepare(root.path(), &targets, UpdateOptions::default())
        .unwrap();
    assert!(
        refused
            .validation
            .iter()
            .any(|v| v.contains("changed since depsmith installed it")
                && v.contains("depsmith init")),
        "{:?}",
        refused.validation
    );
    assert!(!refused.validation.iter().any(|v| v == "tampered"));
    let mut asked = 0;
    let restored = engine
        .init(root.path(), &UpdateOptions::default(), &mut |_| {
            asked += 1;
            true
        })
        .unwrap();
    assert_eq!(asked, 1);
    assert_eq!(restored["installed"], serde_json::json!([TOOL]));
    assert_eq!(*requests.lock().unwrap(), 2);
    let fetched = engine
        .prepare(root.path(), &targets, UpdateOptions::default())
        .unwrap();
    assert!(fetched.validation.iter().any(|v| v == "fetched tool ran"));
}

fn targets_of(engine: &Engine, root: &Path) -> Vec<Target> {
    engine.discover(root).unwrap()
}

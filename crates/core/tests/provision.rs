//! Tool provisioning offline: pinned downloads from a local HTTP server are
//! checksum-verified before extraction, unpacked into the tool cache, and
//! never written when verification or path safety fails.
use depsmith_core::provision::{self, Archive, ToolDownload};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
};

/// Serve `routes` (path -> body) over HTTP on 127.0.0.1, counting requests
/// in the returned log. Other paths get 404.
fn serve(routes: Vec<(&str, Vec<u8>)>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes: BTreeMap<String, Vec<u8>> =
        routes.into_iter().map(|(p, b)| (p.to_owned(), b)).collect();
    let log = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let seen = log.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap_or_default();
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
            seen.lock().unwrap().push(path.clone());
            let (status, body) = match routes.get(&path) {
                Some(body) => ("200 OK", body.clone()),
                None => ("404 Not Found", vec![]),
            };
            let mut stream = &stream;
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    (base, log)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

const PROGRAM: &[u8] = b"#!/bin/sh\necho 'tool 1.2.3'\n";

fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(vec![], flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    for (path, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        // Bypass the builder's path checks to model hostile archives.
        let name = header.as_old_mut().name.as_mut();
        name[..path.len()].copy_from_slice(path.as_bytes());
        header.set_cksum();
        builder.append(&header, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(vec![]));
    for (path, data) in entries {
        writer
            .start_file(*path, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn download(
    base: &str,
    path: &str,
    bytes: &[u8],
    archive: Archive,
    executable: &str,
) -> ToolDownload {
    ToolDownload {
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        url: format!("{base}{path}"),
        sha256: sha256(bytes),
        archive,
        executable: executable.into(),
        ..Default::default()
    }
}

fn installed(cache: &Path, d: &ToolDownload) -> depsmith_core::Result<Vec<u8>> {
    provision::install("tool", "1.2.3", d, cache, 30).map(|p| {
        assert!(
            p.starts_with(cache.join("tool").join("1.2.3")),
            "{}",
            p.display()
        );
        fs::read(p).unwrap()
    })
}

#[test]
fn archives_and_binaries_install_into_the_versioned_cache() {
    let tarball = tar_gz(&[("tool-x/tool", PROGRAM), ("tool-x/README", b"r")]);
    let zipped = zip(&[("tool.exe", PROGRAM)]);
    let (base, _) = serve(vec![
        ("/t.tar.gz", tarball.clone()),
        ("/t.zip", zipped.clone()),
        ("/tool-bin", PROGRAM.to_vec()),
    ]);
    for d in [
        download(&base, "/t.tar.gz", &tarball, Archive::TarGz, "tool-x/tool"),
        download(&base, "/t.zip", &zipped, Archive::Zip, "tool.exe"),
        download(&base, "/tool-bin", PROGRAM, Archive::Binary, "tool"),
    ] {
        let cache = tempfile::tempdir().unwrap();
        assert_eq!(installed(cache.path(), &d).unwrap(), PROGRAM, "{}", d.url);
        let found = provision::cached("tool", "1.2.3", &d, cache.path());
        assert!(found.is_some(), "{}", d.url);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(found.unwrap()).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "{}", d.url);
        }
    }
}

#[test]
fn a_checksum_mismatch_installs_nothing() {
    let (base, _) = serve(vec![("/tool-bin", PROGRAM.to_vec())]);
    let mut d = download(&base, "/tool-bin", PROGRAM, Archive::Binary, "tool");
    d.sha256 = sha256(b"something else");
    let cache = tempfile::tempdir().unwrap();
    let error = installed(cache.path(), &d).unwrap_err().to_string();
    assert!(error.contains("sha256"), "{error}");
    assert!(provision::cached("tool", "1.2.3", &d, cache.path()).is_none());
    assert!(!cache.path().join("tool").join("1.2.3").exists());
}

#[test]
fn archive_entries_escaping_the_cache_are_refused() {
    // The executable is fine; another entry tries to leave the directory.
    let entries: [(&str, &[u8]); 2] = [("../../escaped", PROGRAM), ("tool", PROGRAM)];
    let tarball = tar_gz(&entries);
    let zipped = zip(&[("../escaped.exe", PROGRAM), ("tool.exe", PROGRAM)]);
    let (base, _) = serve(vec![
        ("/t.tar.gz", tarball.clone()),
        ("/t.zip", zipped.clone()),
    ]);
    for d in [
        download(&base, "/t.tar.gz", &tarball, Archive::TarGz, "tool"),
        download(&base, "/t.zip", &zipped, Archive::Zip, "tool.exe"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("a").join("cache");
        fs::create_dir_all(&cache).unwrap();
        let error = installed(&cache, &d).unwrap_err().to_string();
        assert!(
            error.contains("archive entry") && error.contains("escapes"),
            "{error}"
        );
        let leaked: Vec<_> = walk(root.path())
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("escaped"))
            })
            .collect();
        assert!(leaked.is_empty(), "{leaked:?}");
        assert!(provision::cached("tool", "1.2.3", &d, &cache).is_none());
    }
}

#[test]
fn an_executable_outside_the_archive_root_is_refused() {
    let d = download(
        "https://example.invalid",
        "/t",
        PROGRAM,
        Archive::Binary,
        "../tool",
    );
    let cache = tempfile::tempdir().unwrap();
    let error = installed(cache.path(), &d).unwrap_err().to_string();
    assert!(
        error.contains("executable") && error.contains("escapes"),
        "{error}"
    );
}

#[test]
fn an_incomplete_earlier_install_is_replaced() {
    let (base, _) = serve(vec![("/tool-bin", PROGRAM.to_vec())]);
    let d = download(&base, "/tool-bin", PROGRAM, Archive::Binary, "tool");
    let cache = tempfile::tempdir().unwrap();
    let stale = cache.path().join("tool").join("1.2.3");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("partial"), b"x").unwrap();
    assert_eq!(installed(cache.path(), &d).unwrap(), PROGRAM);
    assert!(!stale.join("partial").exists());
}

#[test]
fn redirects_must_stay_on_https_or_loopback() {
    use std::io::{BufRead, BufReader, Write};
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
                "HTTP/1.1 302 Found\r\nLocation: http://example.invalid/tool\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    let d = download(&base, "/tool", PROGRAM, Archive::Binary, "tool");
    let cache = tempfile::tempdir().unwrap();
    let error = installed(cache.path(), &d).unwrap_err().to_string();
    assert!(error.contains("https"), "{error}");
}

#[test]
fn only_https_or_loopback_urls_are_fetched() {
    let mut d = download(
        "http://example.invalid",
        "/tool",
        PROGRAM,
        Archive::Binary,
        "tool",
    );
    let cache = tempfile::tempdir().unwrap();
    let error = installed(cache.path(), &d).unwrap_err().to_string();
    assert!(error.contains("https"), "{error}");
    d.url = "https://example.invalid/tool".into();
    assert!(provision::install("tool", "1.2.3", &d, cache.path(), 1).is_err());
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut output = vec![];
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            output.extend(walk(&path));
        }
        output.push(path);
    }
    output
}

#[test]
fn every_pinned_download_is_https_with_a_sha256_for_each_ci_host() {
    let specs = depsmith_core::Engine::default().specs();
    let tools: Vec<_> = specs
        .iter()
        .flat_map(|s| s.tools.clone())
        .chain([depsmith_core::scan::scanner_tool()])
        .collect();
    let downloadable: Vec<&str> = tools
        .iter()
        .filter(|t| !t.downloads.is_empty())
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(
        downloadable,
        ["pixi", "cargo", "conda-lock", "conda", "uv", "npm", "grype"]
    );
    for tool in &tools {
        for d in &tool.downloads {
            // A Python tool comes from its embedded hash-locked requirements
            // through uv, not from a URL of its own.
            if let Some(provision::Setup::UvVenv { lock, .. }) = &d.setup {
                assert!(d.url.is_empty() && d.sha256.is_empty(), "{d:?}");
                assert_eq!(lock, &tool.name);
                continue;
            }
            assert!(d.url.starts_with("https://"), "{}", d.url);
            assert!(
                d.sha256.len() == 64 && d.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "{}: {}",
                tool.name,
                d.sha256
            );
            // The asset is the tested version, or installs it (rustup-init
            // installs the tested toolchain).
            let installs = match &d.setup {
                Some(provision::Setup::Run { args, .. }) => args.contains(&tool.tested_versions[0]),
                _ => false,
            };
            // npm is bundled with a pinned Node.js, whose release the URL names.
            let bundled = tool.name == "npm" && d.url.starts_with("https://nodejs.org/dist/v");
            assert!(
                d.url.contains(&tool.tested_versions[0]) || installs || bundled,
                "{} is not the tested {}",
                d.url,
                tool.tested_versions[0]
            );
        }
        if tool.downloads.is_empty() {
            continue;
        }
        for (os, arch) in [
            ("linux", "x86_64"),
            ("macos", "aarch64"),
            ("windows", "x86_64"),
        ] {
            assert!(
                tool.downloads.iter().any(|d| d.os == os && d.arch == arch),
                "{} has no {os}-{arch} download",
                tool.name
            );
        }
    }
}

fn spec(version: &str, d: &ToolDownload) -> depsmith_core::adapter::ToolSpec {
    depsmith_core::adapter::ToolSpec {
        name: "tool".into(),
        default: "tool".into(),
        tested_versions: vec![version.into()],
        downloads: vec![d.clone()],
    }
}

#[test]
fn recorded_installs_are_trusted_only_while_unchanged() {
    use provision::Trust;
    let (base, log) = serve(vec![("/tool-bin", PROGRAM.to_vec())]);
    let d = download(&base, "/tool-bin", PROGRAM, Archive::Binary, "tool");
    let (repo, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let tool = spec("1.2.3", &d);
    assert_eq!(
        provision::trust(repo.path(), &tool, cache.path()),
        Trust::Absent
    );

    let path = provision::reinstall(repo.path(), &tool, cache.path(), 30).unwrap();
    assert_eq!(
        provision::trust(repo.path(), &tool, cache.path()),
        Trust::Verified(path.clone())
    );
    assert_eq!(
        fs::read_to_string(repo.path().join(".depsmith/.gitignore")).unwrap(),
        "*\n"
    );

    // Another repository sharing the cache has no record of it.
    let other = tempfile::tempdir().unwrap();
    assert_eq!(
        provision::trust(other.path(), &tool, cache.path()),
        Trust::Unrecorded
    );

    // A newer pinned version is not the recorded install.
    match provision::trust(repo.path(), &spec("1.2.4", &d), cache.path()) {
        Trust::Untrusted(reason) => assert!(reason.contains("1.2.3"), "{reason}"),
        other => panic!("{other:?}"),
    }

    // A changed executable is not trusted, and reinstalling replaces it with
    // a fresh download instead of re-recording the changed copy.
    fs::write(&path, b"#!/bin/sh\necho tampered\n").unwrap();
    match provision::trust(repo.path(), &tool, cache.path()) {
        Trust::Untrusted(reason) => assert!(reason.contains("changed"), "{reason}"),
        other => panic!("{other:?}"),
    }
    let requests = log.lock().unwrap().len();
    provision::reinstall(repo.path(), &tool, cache.path(), 30).unwrap();
    assert_eq!(log.lock().unwrap().len(), requests + 1);
    assert_eq!(fs::read(&path).unwrap(), PROGRAM);
    assert_eq!(
        provision::trust(repo.path(), &tool, cache.path()),
        Trust::Verified(path)
    );
}

#[test]
fn a_checksum_mismatch_is_an_operation_failure() {
    let (base, _) = serve(vec![("/tool-bin", PROGRAM.to_vec())]);
    let mut d = download(&base, "/tool-bin", PROGRAM, Archive::Binary, "tool");
    d.sha256 = sha256(b"other");
    let cache = tempfile::tempdir().unwrap();
    let error = provision::install("tool", "1.2.3", &d, cache.path(), 30).unwrap_err();
    assert!(
        matches!(error, depsmith_core::Error::Operation(_)),
        "{error:?}"
    );
}

#[test]
fn redirects_to_plain_http_are_refused_even_locally() {
    let (target, _) = serve(vec![("/tool-bin", PROGRAM.to_vec())]);
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
                "HTTP/1.1 302 Found\r\nLocation: {target}/tool-bin\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    let d = download(&base, "/tool", PROGRAM, Archive::Binary, "tool");
    let cache = tempfile::tempdir().unwrap();
    let error = installed(cache.path(), &d).unwrap_err().to_string();
    assert!(error.contains("redirect"), "{error}");
}

#[test]
fn reinstalling_an_identical_cached_copy_keeps_it_in_place() {
    let (base, log) = serve(vec![("/tool-bin", PROGRAM.to_vec())]);
    let d = download(&base, "/tool-bin", PROGRAM, Archive::Binary, "tool");
    let cache = tempfile::tempdir().unwrap();
    let tool = spec("1.2.3", &d);
    let first = tempfile::tempdir().unwrap();
    let path = provision::reinstall(first.path(), &tool, cache.path(), 30).unwrap();
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(&fs::metadata(&path).unwrap());
    // A second repository verifies against a fresh download, then records
    // the same copy instead of replacing a file others may be running.
    let second = tempfile::tempdir().unwrap();
    assert_eq!(
        provision::trust(second.path(), &tool, cache.path()),
        provision::Trust::Unrecorded
    );
    let again = provision::reinstall(second.path(), &tool, cache.path(), 30).unwrap();
    assert_eq!(again, path);
    #[cfg(unix)]
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&fs::metadata(&path).unwrap()),
        inode,
        "the identical copy was replaced"
    );
    assert_eq!(log.lock().unwrap().len(), 2);
    assert_eq!(
        provision::trust(second.path(), &tool, cache.path()),
        provision::Trust::Verified(path)
    );
}

#[cfg(unix)]
#[test]
fn records_are_never_written_through_a_symlink() {
    let (base, _) = serve(vec![("/tool-bin", PROGRAM.to_vec())]);
    let d = download(&base, "/tool-bin", PROGRAM, Archive::Binary, "tool");
    let (repo, cache, elsewhere) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    std::os::unix::fs::symlink(elsewhere.path(), repo.path().join(".depsmith")).unwrap();
    let error = provision::reinstall(repo.path(), &spec("1.2.3", &d), cache.path(), 30)
        .unwrap_err()
        .to_string();
    assert!(error.contains("symlink"), "{error}");
    assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[cfg(unix)]
mod installers {
    use super::*;
    use provision::Setup;

    /// An installer that writes `{prefix}/tool/bin/tool` from its first
    /// argument and records its environment.
    const INSTALLER: &[u8] = b"#!/bin/sh\nset -e\nmkdir -p \"$1/tool/bin\"\nprintf '#!/bin/sh\\necho installed\\n' > \"$1/tool/bin/tool\"\nchmod 755 \"$1/tool/bin/tool\"\necho \"$TOOL_HOME\" > \"$1/home\"\n";

    fn installer(base: &str, bytes: &[u8]) -> ToolDownload {
        ToolDownload {
            setup: Some(Setup::Run {
                installer: "tool-init".into(),
                args: vec!["{prefix}".into()],
                env: [("TOOL_HOME".to_owned(), "{prefix}/tool".to_owned())].into(),
            }),
            env: [("TOOL_HOME".to_owned(), "{prefix}/tool".to_owned())].into(),
            path: vec!["tool/bin".into()],
            ..download(base, "/tool-init", bytes, Archive::Binary, "tool/bin/tool")
        }
    }

    #[test]
    fn an_installer_runs_in_the_version_directory() {
        let (base, _) = serve(vec![("/tool-init", INSTALLER.to_vec())]);
        let d = installer(&base, INSTALLER);
        let cache = tempfile::tempdir().unwrap();
        let path = provision::install("tool", "1.2.3", &d, cache.path(), 30).unwrap();
        let prefix = cache.path().join("tool").join("1.2.3");
        assert_eq!(path, prefix.join("tool/bin/tool"));
        assert!(path.is_file());
        // The installer ran in the staging directory with expanded arguments.
        assert!(fs::read_to_string(prefix.join("home"))
            .unwrap()
            .trim()
            .ends_with("/tool"));
        let env = provision::runtime_env(&d, &path);
        assert_eq!(
            env.iter()
                .find(|(k, _)| k == "TOOL_HOME")
                .map(|(_, v)| v.clone()),
            Some(prefix.join("tool").to_string_lossy().into_owned())
        );
        let path_var = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.clone())
            .unwrap();
        assert!(
            path_var.starts_with(&*prefix.join("tool/bin").to_string_lossy()),
            "{path_var}"
        );
    }

    #[test]
    fn a_failing_or_incomplete_installer_installs_nothing() {
        let broken: &[u8] = b"#!/bin/sh\nexit 3\n";
        let silent: &[u8] = b"#!/bin/sh\nexit 0\n";
        let (base, _) = serve(vec![
            ("/broken", broken.to_vec()),
            ("/silent", silent.to_vec()),
        ]);
        for (path, bytes, needle) in [
            ("/broken", broken, "installer"),
            ("/silent", silent, "tool/bin/tool"),
        ] {
            let mut d = installer(&base, bytes);
            d.url = format!("{base}{path}");
            let cache = tempfile::tempdir().unwrap();
            let error = provision::install("tool", "1.2.3", &d, cache.path(), 30)
                .unwrap_err()
                .to_string();
            assert!(error.contains(needle), "{error}");
            assert!(!cache.path().join("tool").join("1.2.3").exists());
        }
    }

    #[test]
    fn a_changed_install_tree_is_detected_and_repaired() {
        let (base, _) = serve(vec![("/tool-init", INSTALLER.to_vec())]);
        let tool = depsmith_core::adapter::ToolSpec {
            name: "tool".into(),
            default: "tool".into(),
            tested_versions: vec!["1.2.3".into()],
            downloads: vec![installer(&base, INSTALLER)],
        };
        let (repo, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        provision::reinstall(repo.path(), &tool, cache.path(), 30).unwrap();
        assert_eq!(provision::verify_tree(repo.path(), &tool), None);
        // The entry point is untouched; a file beside it changes.
        let home = cache.path().join("tool/1.2.3/home");
        fs::write(&home, "tampered").unwrap();
        let reason = provision::verify_tree(repo.path(), &tool).unwrap();
        assert!(reason.contains("changed"), "{reason}");
        provision::reinstall(repo.path(), &tool, cache.path(), 30).unwrap();
        assert_ne!(fs::read_to_string(&home).unwrap(), "tampered");
        assert_eq!(provision::verify_tree(repo.path(), &tool), None);
    }

    #[test]
    fn cargo_keeps_the_users_cargo_home() {
        for d in &provision::pinned("cargo") {
            assert!(d.env.contains_key("RUSTUP_HOME"), "{d:?}");
            assert!(!d.env.contains_key("CARGO_HOME"), "{d:?}");
        }
    }
}

#[cfg(unix)]
mod python_tools {
    use super::*;
    use provision::Setup;
    use std::collections::BTreeMap;

    /// A uv that makes `venv DIR` a directory with bin/python and, for
    /// `pip install --python DIR ... -r FILE`, keeps FILE and adds the tool.
    fn stub_uv(dir: &Path) -> String {
        let uv = dir.join("uv");
        fs::write(
            &uv,
            "#!/bin/sh\nset -e\ncase \"$1\" in\n  venv) for a; do last=$a; done; mkdir -p \"$last/bin\"; echo \"$*\" > \"$last/venv-args\";;\n  pip) shift 2; venv=\"\"; req=\"\"; flags=\"\"; while [ $# -gt 0 ]; do case \"$1\" in --python) venv=$2; shift 2;; -r) req=$2; shift 2;; *) flags=\"$flags $1\"; shift;; esac; done; echo \"$flags\" > \"$venv/pip-flags\"; cp \"$req\" \"$venv/requirements\"; printf '#!/bin/sh\\necho conda-lock 4.0.2\\n' > \"$venv/bin/conda-lock\"; chmod 755 \"$venv/bin/conda-lock\";;\n  *) exit 2;;\nesac\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&uv, fs::Permissions::from_mode(0o755)).unwrap();
        uv.to_string_lossy().into_owned()
    }

    fn conda_lock() -> ToolDownload {
        provision::host_download(&depsmith_core::adapter::ToolSpec {
            name: "conda-lock".into(),
            default: "conda-lock".into(),
            tested_versions: vec!["4.0.2".into()],
            downloads: provision::pinned("conda-lock"),
        })
        .expect("conda-lock for this host")
        .clone()
    }

    #[test]
    fn python_tools_install_hash_locked_into_a_relocatable_venv() {
        let d = conda_lock();
        assert!(matches!(d.setup, Some(Setup::UvVenv { .. })), "{d:?}");
        let (stub, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let tools = BTreeMap::from([("uv".to_owned(), stub_uv(stub.path()))]);
        let path =
            provision::install_with("conda-lock", "4.0.2", &d, cache.path(), 30, &tools).unwrap();
        let venv = cache.path().join("conda-lock/4.0.2/venv");
        assert_eq!(path, venv.join("bin/conda-lock"));
        let venv_args = fs::read_to_string(venv.join("venv-args")).unwrap();
        for flag in ["--relocatable", "--no-config", "--managed-python"] {
            assert!(venv_args.contains(flag), "{flag}: {venv_args}");
        }
        let flags = fs::read_to_string(venv.join("pip-flags")).unwrap();
        assert!(
            flags.contains("--require-hashes") && flags.contains("--no-deps"),
            "{flags}"
        );
        let requirements = fs::read_to_string(venv.join("requirements")).unwrap();
        assert!(requirements.contains("conda-lock==4.0.2"), "{requirements}");
        assert!(requirements.contains("--hash=sha256:"));
    }

    #[test]
    fn python_tools_need_uv() {
        let cache = tempfile::tempdir().unwrap();
        let error = provision::install_with(
            "conda-lock",
            "4.0.2",
            &conda_lock(),
            cache.path(),
            30,
            &BTreeMap::new(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("needs uv"), "{error}");
    }
}

#[cfg(unix)]
mod symlinks {
    use super::*;

    fn with_link(link: &str, target: &str) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(vec![], flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let mut file = tar::Header::new_gnu();
        file.set_size(PROGRAM.len() as u64);
        file.set_mode(0o755);
        file.set_cksum();
        builder
            .append_data(&mut file, "tool-x/lib/real-tool", PROGRAM)
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        builder.append_link(&mut header, link, target).unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn links_inside_the_archive_are_kept() {
        let tarball = with_link("tool-x/bin/tool", "../lib/real-tool");
        let (base, _) = serve(vec![("/t.tar.gz", tarball.clone())]);
        let d = download(
            &base,
            "/t.tar.gz",
            &tarball,
            Archive::TarGz,
            "tool-x/bin/tool",
        );
        let cache = tempfile::tempdir().unwrap();
        assert_eq!(installed(cache.path(), &d).unwrap(), PROGRAM);
    }

    #[test]
    fn links_leaving_the_archive_are_refused() {
        let tarball = with_link("tool-x/bin/tool", "../../../../etc/passwd");
        let (base, _) = serve(vec![("/t.tar.gz", tarball.clone())]);
        let d = download(
            &base,
            "/t.tar.gz",
            &tarball,
            Archive::TarGz,
            "tool-x/lib/real-tool",
        );
        let cache = tempfile::tempdir().unwrap();
        let error = installed(cache.path(), &d).unwrap_err().to_string();
        assert!(error.contains("escapes"), "{error}");
        assert!(!cache.path().join("tool").join("1.2.3").exists());
    }

    #[test]
    fn links_cannot_escape_through_earlier_links() {
        let encoder = flate2::write::GzEncoder::new(vec![], flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (link, target) in [("a/b/d", ".."), ("a/b/d/e", "../../..")] {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            builder.append_link(&mut header, link, target).unwrap();
        }
        let mut file = tar::Header::new_gnu();
        file.set_size(PROGRAM.len() as u64);
        file.set_mode(0o755);
        file.set_cksum();
        builder.append_data(&mut file, "tool", PROGRAM).unwrap();
        let tarball = builder.into_inner().unwrap().finish().unwrap();
        let (base, _) = serve(vec![("/t.tar.gz", tarball.clone())]);
        let d = download(&base, "/t.tar.gz", &tarball, Archive::TarGz, "tool");
        let cache = tempfile::tempdir().unwrap();
        let error = installed(cache.path(), &d).unwrap_err().to_string();
        assert!(error.contains("through a link"), "{error}");
    }
}

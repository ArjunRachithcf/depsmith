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
    assert_eq!(downloadable, ["pixi", "conda", "uv", "grype"]);
    for tool in &tools {
        for d in &tool.downloads {
            assert!(d.url.starts_with("https://"), "{}", d.url);
            assert!(
                d.sha256.len() == 64 && d.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "{}: {}",
                tool.name,
                d.sha256
            );
            assert!(
                d.url.contains(&tool.tested_versions[0]),
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

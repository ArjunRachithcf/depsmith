//! Downloading a missing native tool, only with consent: the pinned tested
//! release is fetched over HTTPS, its sha256 checked before anything is
//! unpacked, and the executable installed into a per-version tool cache.
//! Nothing is downloaded unless a caller asks for it.
use crate::{adapter::ToolSpec, Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// How a release asset packages its executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Archive {
    /// A gzip-compressed tarball.
    TarGz,
    /// A zip archive.
    Zip,
    /// The executable itself.
    Binary,
}

/// A pinned release asset of a tool for one host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDownload {
    /// Host operating system, as [`std::env::consts::OS`] (`linux`, `macos`,
    /// `windows`).
    pub os: String,
    /// Host architecture, as [`std::env::consts::ARCH`] (`x86_64`, `aarch64`).
    pub arch: String,
    /// HTTPS URL of the asset.
    pub url: String,
    /// Expected sha256 of the asset, in lowercase hex.
    pub sha256: String,
    /// How the asset packages the executable.
    pub archive: Archive,
    /// Path of the executable inside the archive (its file name for a
    /// [`Archive::Binary`]).
    pub executable: String,
}

/// The tool cache: `DEPSMITH_TOOLS_DIR`, else `depsmith/tools` under the
/// user's cache directory (`XDG_CACHE_HOME` or `~/.cache`, `~/Library/Caches`
/// on macOS, `%LOCALAPPDATA%` on Windows).
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DEPSMITH_TOOLS_DIR").filter(|d| !d.is_empty()) {
        return Some(dir.into());
    }
    let env = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        env("LOCALAPPDATA")?
    } else if cfg!(target_os = "macos") {
        env("HOME")?.join("Library").join("Caches")
    } else {
        env("XDG_CACHE_HOME").or_else(|| Some(env("HOME")?.join(".cache")))?
    };
    Some(base.join("depsmith").join("tools"))
}

/// The pinned download of `tool` for this host, if it has one.
pub fn host_download(tool: &ToolSpec) -> Option<&ToolDownload> {
    tool.downloads
        .iter()
        .find(|d| d.os == std::env::consts::OS && d.arch == std::env::consts::ARCH)
}

/// A relative path with only normal components, or an error naming `what`.
fn contained(path: &Path, what: &str) -> Result<PathBuf> {
    if !crate::working_tree::contained(path) {
        return Err(Error::Invalid(format!(
            "{what} {} escapes the tool directory",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

fn version_dir(name: &str, version: &str, cache: &Path) -> Result<PathBuf> {
    Ok(cache.join(contained(&Path::new(name).join(version), "tool")?))
}

/// The installed executable of `download` for `name` `version` in `cache`,
/// if it is there.
pub fn cached(name: &str, version: &str, download: &ToolDownload, cache: &Path) -> Option<PathBuf> {
    let executable = contained(Path::new(&download.executable), "executable").ok()?;
    let path = version_dir(name, version, cache).ok()?.join(executable);
    path.is_file().then_some(path)
}

/// Whether `url` may be fetched: HTTPS, or plain HTTP to this machine.
fn allowed(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        || (url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
}

fn fetch(url: &str, timeout: u64) -> Result<Vec<u8>> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| Error::Invalid(format!("invalid download URL {url}: {e}")))?;
    if !allowed(&parsed) {
        return Err(Error::Invalid(format!(
            "refusing to download {url}: only https URLs (or http to this machine) are fetched"
        )));
    }
    // Every redirect hop is held to the same rule as the first URL.
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() > 10 {
            attempt.error("too many redirects")
        } else if allowed(attempt.url()) {
            attempt.follow()
        } else {
            let url = attempt.url().to_string();
            attempt.error(format!(
                "refusing a redirect to {url}: only https URLs are fetched"
            ))
        }
    });
    let client = crate::http::build(crate::http::builder(timeout).redirect(policy))?;
    let response = client.get(parsed).send().map_err(|e| {
        use std::error::Error as _;
        let cause = e.source().map(|c| format!(": {c}")).unwrap_or_default();
        Error::Operation(format!("download failed: {}{cause}", e.without_url()))
    })?;
    if !response.status().is_success() {
        return Err(Error::Operation(format!(
            "{url}: HTTP {}",
            response.status()
        )));
    }
    Ok(response
        .bytes()
        .map_err(|e| Error::Operation(format!("download failed: {}", e.without_url())))?
        .to_vec())
}

fn write_entry(out: &Path, relative: &Path, reader: &mut dyn Read) -> Result<()> {
    let path = out.join(contained(relative, "archive entry")?);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    std::io::copy(reader, &mut fs::File::create(path)?)?;
    Ok(())
}

/// Unpack the regular files of `bytes` into `out`, refusing any entry whose
/// path would leave it.
fn unpack(bytes: &[u8], download: &ToolDownload, out: &Path) -> Result<()> {
    let unreadable = |e: &dyn std::fmt::Display| {
        Error::Operation(format!("unreadable archive {}: {e}", download.url))
    };
    match download.archive {
        Archive::Binary => {
            let name = Path::new(&download.executable);
            write_entry(out, name, &mut &bytes[..])
        }
        Archive::TarGz => {
            let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
            for entry in archive.entries().map_err(|e| unreadable(&e))? {
                let mut entry = entry.map_err(|e| unreadable(&e))?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let path = entry.path().map_err(|e| unreadable(&e))?.into_owned();
                write_entry(out, &path, &mut entry)?;
            }
            Ok(())
        }
        Archive::Zip => {
            let mut archive =
                zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| unreadable(&e))?;
            for index in 0..archive.len() {
                let mut entry = archive.by_index(index).map_err(|e| unreadable(&e))?;
                if !entry.is_file() {
                    continue;
                }
                let path = PathBuf::from(entry.name());
                write_entry(out, &path, &mut entry)?;
            }
            Ok(())
        }
    }
}

/// Download `download` for `name` `version` into `cache` and return the
/// executable, reusing an earlier install. The asset is checked against its
/// sha256 before it is unpacked, and the version directory appears only once
/// complete.
///
/// # Errors
///
/// Fails for a URL that is neither HTTPS nor loopback, a failed download, a
/// checksum mismatch, an unreadable archive, an entry escaping the tool
/// directory, or an archive without the executable.
pub fn install(
    name: &str,
    version: &str,
    download: &ToolDownload,
    cache: &Path,
    timeout: u64,
) -> Result<PathBuf> {
    let executable = contained(Path::new(&download.executable), "executable")?;
    if let Some(path) = cached(name, version, download, cache) {
        return Ok(path);
    }
    let bytes = fetch(&download.url, timeout)?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(&download.sha256) {
        return Err(Error::Policy(format!(
            "{} does not match its pinned sha256 (expected {}, got {actual}); nothing was installed",
            download.url, download.sha256
        )));
    }
    let target = version_dir(name, version, cache)?;
    let parent = target.parent().expect("a version directory has a parent");
    fs::create_dir_all(parent)?;
    let staging = tempfile::tempdir_in(parent)?;
    let out = staging.path().join("out");
    fs::create_dir(&out)?;
    unpack(&bytes, download, &out)?;
    let program = out.join(&executable);
    if !program.is_file() {
        return Err(Error::Operation(format!(
            "{} has no {}",
            download.url,
            executable.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
    }
    if target.exists() && !target.join(&executable).is_file() {
        // An earlier install stopped part-way; replace it.
        fs::remove_dir_all(&target)?;
    }
    match fs::rename(&out, &target) {
        // Another process finished the same install first.
        Err(_) if target.join(&executable).is_file() => {}
        result => result?,
    }
    Ok(target.join(executable))
}

/// Pinned assets as (os, arch, url, sha256, archive, executable).
type Asset = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Archive,
    &'static str,
);

const UV: &[Asset] = &[
    ("linux", "x86_64", "https://github.com/astral-sh/uv/releases/download/0.12.15/uv-x86_64-unknown-linux-musl.tar.gz", "999c0c3da986953e508985c3932d283d2c62eb167b4f8d81e79f565e34104959", Archive::TarGz, "uv-x86_64-unknown-linux-musl/uv"),
    ("linux", "aarch64", "https://github.com/astral-sh/uv/releases/download/0.12.15/uv-aarch64-unknown-linux-musl.tar.gz", "93b801abb146e6431fb0434346a0162e65d3f0d1cd7360144d04c43488fd7f7d", Archive::TarGz, "uv-aarch64-unknown-linux-musl/uv"),
    ("macos", "x86_64", "https://github.com/astral-sh/uv/releases/download/0.12.15/uv-x86_64-apple-darwin.tar.gz", "e9ca61775532368fe518ab03e7a354c7ecab8ccb3c7d941c775fcc4a362b801b", Archive::TarGz, "uv-x86_64-apple-darwin/uv"),
    ("macos", "aarch64", "https://github.com/astral-sh/uv/releases/download/0.12.15/uv-aarch64-apple-darwin.tar.gz", "dc304b9ed1b24174572290fba60ac3f6fe63c73a671f0439e62a91375841964d", Archive::TarGz, "uv-aarch64-apple-darwin/uv"),
    ("windows", "x86_64", "https://github.com/astral-sh/uv/releases/download/0.12.15/uv-x86_64-pc-windows-msvc.zip", "477bd99a84e34891f2bd4c9152ddeb74e971accccbc59c0f0301f11f08a32d46", Archive::Zip, "uv.exe"),
    ("windows", "aarch64", "https://github.com/astral-sh/uv/releases/download/0.12.15/uv-aarch64-pc-windows-msvc.zip", "a37c8e96cb1260488c8510b64c848533a3a82a2fdf9e905de7c2700ceebf6437", Archive::Zip, "uv.exe"),
];

const PIXI: &[Asset] = &[
    ("linux", "x86_64", "https://github.com/prefix-dev/pixi/releases/download/v0.80.0/pixi-x86_64-unknown-linux-musl", "387a2d3052e656f61ccf735e6750255451366f45635a2da09116b1f8394b2837", Archive::Binary, "pixi"),
    ("linux", "aarch64", "https://github.com/prefix-dev/pixi/releases/download/v0.80.0/pixi-aarch64-unknown-linux-musl", "20e9fcfffa1ef02d10b7a5df79092110c1e4aa9caa13072f5008a2fd2ecd9436", Archive::Binary, "pixi"),
    ("macos", "x86_64", "https://github.com/prefix-dev/pixi/releases/download/v0.80.0/pixi-x86_64-apple-darwin", "e2c9b1950c217dbdd191e3248bc5629486cda5b9d6c092d32a942ddb0c8bf4de", Archive::Binary, "pixi"),
    ("macos", "aarch64", "https://github.com/prefix-dev/pixi/releases/download/v0.80.0/pixi-aarch64-apple-darwin", "e1af87edbd2ae2a986efe5b7716b80d35b0c99ac33b60d23ff232413b527ca8b", Archive::Binary, "pixi"),
    ("windows", "x86_64", "https://github.com/prefix-dev/pixi/releases/download/v0.80.0/pixi-x86_64-pc-windows-msvc.exe", "ebf870ab0be4abad5e3b1e083d4dd8aeecbbc37f84e3070da42a4db6be4c6c91", Archive::Binary, "pixi.exe"),
    ("windows", "aarch64", "https://github.com/prefix-dev/pixi/releases/download/v0.80.0/pixi-aarch64-pc-windows-msvc.exe", "0bd4b7a87ed5814373b7c1f3b0d7d5ab5183035a9eb29696983b674ec71aa313", Archive::Binary, "pixi.exe"),
];

const GRYPE: &[Asset] = &[
    ("linux", "x86_64", "https://github.com/anchore/grype/releases/download/v0.119.0/grype_0.119.0_linux_amd64.tar.gz", "3fa2dc4b924621ab65404cf08d0b8438d896d80ab949c9d5a4ca283c36004c9b", Archive::TarGz, "grype"),
    ("linux", "aarch64", "https://github.com/anchore/grype/releases/download/v0.119.0/grype_0.119.0_linux_arm64.tar.gz", "29f0ec7c549ddb0e2b6a0ca714851f7399438afc399b80c12808e065edc9a8f8", Archive::TarGz, "grype"),
    ("macos", "x86_64", "https://github.com/anchore/grype/releases/download/v0.119.0/grype_0.119.0_darwin_amd64.tar.gz", "ea106d3ab9573d654871ad9e3e89be2237506ff3f8c170e5aeacb59da4def2b8", Archive::TarGz, "grype"),
    ("macos", "aarch64", "https://github.com/anchore/grype/releases/download/v0.119.0/grype_0.119.0_darwin_arm64.tar.gz", "500c9b2b6c089d21481815f57a553fabbd441ec7d1e79d95e3aaf40c3bfc7e36", Archive::TarGz, "grype"),
    ("windows", "x86_64", "https://github.com/anchore/grype/releases/download/v0.119.0/grype_0.119.0_windows_amd64.zip", "1db5c23b8ba0038a04acebed9c17945e1ade68d9f83e2fe1c101e4fb1feb9a48", Archive::Zip, "grype.exe"),
];

/// micromamba, the conda solver the conda adapter drives as `conda`.
const MICROMAMBA: &[Asset] = &[
    ("linux", "x86_64", "https://github.com/mamba-org/micromamba-releases/releases/download/2.9.0-0/micromamba-linux-64", "366cd9cd8be14df1ab8ed50352a82111082a36686b2d389fdb79a92c3fafb3e3", Archive::Binary, "micromamba"),
    ("linux", "aarch64", "https://github.com/mamba-org/micromamba-releases/releases/download/2.9.0-0/micromamba-linux-aarch64", "9f93b974adcb4d166996af969b6cd371287d1a3e52733704727884d9b74cb7a7", Archive::Binary, "micromamba"),
    ("macos", "x86_64", "https://github.com/mamba-org/micromamba-releases/releases/download/2.9.0-0/micromamba-osx-64", "1e71054bb3ac9a076e21f7ec48acfef536f9b3f1408f371a942784bf5ef83d8a", Archive::Binary, "micromamba"),
    ("macos", "aarch64", "https://github.com/mamba-org/micromamba-releases/releases/download/2.9.0-0/micromamba-osx-arm64", "ec2a072f028e1a7cf20f3e2e74d5a8127cf5a5f27636375b5359811565f4e5be", Archive::Binary, "micromamba"),
    ("windows", "x86_64", "https://github.com/mamba-org/micromamba-releases/releases/download/2.9.0-0/micromamba-win-64.exe", "a6d804394b2418991c4e29562853eaace2f2ce9d9da661a98e74e02e8dbb44b0", Archive::Binary, "micromamba.exe"),
    ("windows", "aarch64", "https://github.com/mamba-org/micromamba-releases/releases/download/2.9.0-0/micromamba-win-arm64.exe", "f0da836d2398c00ac0b43e01f7581ba3430224a04405075c39eb3dd78bf0339a", Archive::Binary, "micromamba.exe"),
];

/// The pinned downloads of the tool named `name` (its first tested
/// version); empty for tools depsmith does not install, such as cargo
/// (rustup's job) and conda-lock (a Python package).
pub fn pinned(name: &str) -> Vec<ToolDownload> {
    let assets = match name {
        "uv" => UV,
        "pixi" => PIXI,
        "grype" => GRYPE,
        "conda" => MICROMAMBA,
        _ => &[],
    };
    assets
        .iter()
        .map(
            |&(os, arch, url, sha256, archive, executable)| ToolDownload {
                os: os.into(),
                arch: arch.into(),
                url: url.into(),
                sha256: sha256.into(),
                archive,
                executable: executable.into(),
            },
        )
        .collect()
}

/// What a repository recorded about a tool `init` installed for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ToolRecord {
    version: String,
    executable: PathBuf,
    sha256: String,
}

/// The repository's records, in `.depsmith/tools.json` (local state; the
/// directory ignores itself in Git).
const RECORDS: &str = ".depsmith/tools.json";

fn read_records(root: &Path) -> std::collections::BTreeMap<String, ToolRecord> {
    fs::read(root.join(RECORDS))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn file_sha256(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

/// Whether a repository can run a tool from the tool cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// Installed for this repository and unchanged since: run this path.
    Verified(PathBuf),
    /// In the tool cache, but this repository never recorded installing it.
    Unrecorded,
    /// Recorded, but no longer the install depsmith made or pins; the reason
    /// says what changed.
    Untrusted(String),
    /// Neither recorded nor in the tool cache.
    Absent,
}

/// Check the tool cache entry of `tool` against the record in `root`: the
/// version depsmith pins, the executable's presence and its sha256.
pub fn trust(root: &Path, tool: &ToolSpec, cache: &Path) -> Trust {
    let version = tool.tested_versions.first().map_or("", String::as_str);
    let Some(record) = read_records(root).remove(&tool.name) else {
        let cached = host_download(tool).and_then(|d| cached(&tool.name, version, d, cache));
        return if cached.is_some() {
            Trust::Unrecorded
        } else {
            Trust::Absent
        };
    };
    if record.version != version {
        return Trust::Untrusted(format!(
            "{} {} was installed, but depsmith now uses {version}",
            tool.name, record.version
        ));
    }
    match file_sha256(&record.executable) {
        Err(_) => Trust::Untrusted(format!(
            "the installed {} at {} is gone",
            tool.name,
            record.executable.display()
        )),
        Ok(sum) if sum != record.sha256 => Trust::Untrusted(format!(
            "the installed {} at {} changed since depsmith installed it",
            tool.name,
            record.executable.display()
        )),
        Ok(_) => Trust::Verified(record.executable),
    }
}

/// Record in `root` that `executable` is the install of `tool`, with its
/// sha256, creating a self-ignoring `.depsmith` directory if needed.
fn record(root: &Path, tool: &ToolSpec, executable: &Path) -> Result<()> {
    let directory = root.join(".depsmith");
    fs::create_dir_all(&directory)?;
    let ignore = directory.join(".gitignore");
    if !ignore.exists() {
        fs::write(&ignore, "*\n")?;
    }
    let mut records = read_records(root);
    records.insert(
        tool.name.clone(),
        ToolRecord {
            version: tool.tested_versions.first().cloned().unwrap_or_default(),
            executable: executable.to_path_buf(),
            sha256: file_sha256(executable)?,
        },
    );
    let bytes = serde_json::to_vec_pretty(&records)
        .map_err(|e| Error::Operation(format!("cannot write tool records: {e}")))?;
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    std::io::Write::write_all(&mut temporary, &bytes)?;
    temporary
        .persist(root.join(RECORDS))
        .map_err(|e| Error::Operation(format!("cannot write tool records: {e}")))?;
    Ok(())
}

/// Download `tool` afresh into `cache` (discarding any cached copy, which
/// cannot be verified once unpacked) and record the install in `root`.
///
/// # Errors
///
/// As for [`install`], plus failures to remove the old copy or to write the
/// record.
pub fn reinstall(root: &Path, tool: &ToolSpec, cache: &Path, timeout: u64) -> Result<PathBuf> {
    let download = host_download(tool).ok_or_else(|| {
        Error::Invalid(format!(
            "{} has no download for {}-{}",
            tool.name,
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    })?;
    let version = tool
        .tested_versions
        .first()
        .ok_or_else(|| Error::Invalid(format!("{} has no tested version to install", tool.name)))?;
    let directory = version_dir(&tool.name, version, cache)?;
    if directory.exists() {
        fs::remove_dir_all(&directory)?;
    }
    let path = install(&tool.name, version, download, cache, timeout)?;
    record(root, tool, &path)?;
    Ok(path)
}

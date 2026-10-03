//! Downloading a missing native tool, only with consent: the pinned tested
//! release is fetched over HTTPS, its sha256 checked before anything is
//! unpacked, and the executable installed into a per-version tool cache.
//! Nothing is downloaded unless a caller asks for it.
use crate::{adapter::ToolSpec, Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// How a release asset packages its executable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Archive {
    /// A gzip-compressed tarball.
    TarGz,
    /// A zip archive.
    Zip,
    /// The executable itself.
    #[default]
    Binary,
}

/// A step that turns a verified download into the installed tool, when
/// unpacking alone does not. `{prefix}` in its strings is the directory being
/// installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Setup {
    /// Run the downloaded installer (a [`Archive::Binary`] saved as
    /// `installer`) with `args` and `env`; it must produce the executable.
    Run {
        /// File name the downloaded installer is saved as.
        installer: String,
        /// Arguments to the installer.
        args: Vec<String>,
        /// Environment for the installer.
        env: BTreeMap<String, String>,
    },
    /// Install a Python tool with uv into a relocatable venv (`venv` under
    /// the install directory) from an embedded, hash-locked requirements
    /// file; nothing is downloaded outside uv's own verified fetches.
    UvVenv {
        /// Name of the embedded lock, such as `conda-lock`.
        lock: String,
        /// Python version for the venv.
        python: String,
    },
}

/// The embedded, universal, hash-locked requirements of the Python tools
/// depsmith installs (`uv pip compile --universal --generate-hashes`).
fn locked_requirements(name: &str) -> Option<&'static str> {
    match name {
        "conda-lock" => Some(include_str!("pins/conda-lock.txt")),
        _ => None,
    }
}

/// A pinned release asset of a tool for one host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// [`Archive::Binary`]), or produced by [`ToolDownload::setup`].
    pub executable: String,
    /// A step after unpacking, such as running an installer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<Setup>,
    /// Environment the tool runs with; `{prefix}` is its install directory.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Directories of the install, relative to it, put first on `PATH` when
    /// the tool runs (for companions such as `rustc` or `node`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
}

/// `text` with `{prefix}` replaced by `prefix`.
fn expand(text: &str, prefix: &Path) -> String {
    text.replace("{prefix}", &prefix.to_string_lossy())
}

/// The environment `download`'s tool runs with, installed with its
/// executable at `executable`: its `env` and, first on `PATH`, its `path`
/// directories, all under the install directory.
pub fn runtime_env(download: &ToolDownload, executable: &Path) -> Vec<(String, String)> {
    let Some(prefix) = install_dir(download, executable) else {
        return vec![];
    };
    let mut env: Vec<(String, String)> = download
        .env
        .iter()
        .map(|(key, value)| (key.clone(), expand(value, prefix)))
        .collect();
    if !download.path.is_empty() {
        let dirs = download.path.iter().map(|dir| prefix.join(dir));
        let current = std::env::var_os("PATH").unwrap_or_default();
        let joined =
            std::env::join_paths(dirs.chain(std::env::split_paths(&current))).unwrap_or(current);
        env.push(("PATH".into(), joined.to_string_lossy().into_owned()));
    }
    env
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
fn confined(path: &Path, what: &str) -> Result<PathBuf> {
    if !crate::working_tree::contained(path) {
        return Err(Error::Invalid(format!(
            "{what} {} escapes the tool directory",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

fn version_dir(name: &str, version: &str, cache: &Path) -> Result<PathBuf> {
    Ok(cache.join(confined(&Path::new(name).join(version), "tool")?))
}

/// The installed executable of `download` for `name` `version` in `cache`,
/// if it is there.
pub fn cached(name: &str, version: &str, download: &ToolDownload, cache: &Path) -> Option<PathBuf> {
    let executable = confined(Path::new(&download.executable), "executable").ok()?;
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
    // Redirects must be HTTPS: only the first URL may be plain HTTP to this
    // machine (local test servers).
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() > 10 {
            attempt.error("too many redirects")
        } else if attempt.url().scheme() == "https" {
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

/// Refuse `relative` when a directory on its way inside `out` is a link an
/// earlier archive entry created: writing through it could leave `out`.
fn not_through_links(out: &Path, relative: &Path) -> Result<()> {
    let mut current = out.to_path_buf();
    for part in relative.parent().into_iter().flat_map(Path::components) {
        current.push(part);
        if fs::symlink_metadata(&current).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Error::Invalid(format!(
                "archive entry {} goes through a link",
                relative.display()
            )));
        }
    }
    Ok(())
}

fn write_entry(out: &Path, relative: &Path, reader: &mut dyn Read) -> Result<()> {
    not_through_links(out, relative)?;
    let path = out.join(confined(relative, "archive entry")?);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    std::io::copy(reader, &mut fs::File::create(path)?)?;
    Ok(())
}

/// Create the archive link `relative` -> `target` under `out` when the target,
/// resolved from the link's directory, stays inside `out` (Node ships
/// `bin/npm` as a link into `lib/`); refuse any other link.
fn write_link(out: &Path, relative: &Path, target: &Path) -> Result<()> {
    let link = confined(relative, "archive entry")?;
    not_through_links(out, relative)?;
    let mut depth: Vec<&std::ffi::OsStr> = vec![];
    for component in link
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .chain(target.components())
    {
        match component {
            std::path::Component::Normal(part) => depth.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if depth.pop().is_some() => {}
            _ => {
                return Err(Error::Invalid(format!(
                    "archive link {} -> {} escapes the tool directory",
                    relative.display(),
                    target.display()
                )))
            }
        }
    }
    let path = out.join(link);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, &path)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err(Error::Operation(format!(
            "archive link {} cannot be created on this platform ({})",
            relative.display(),
            path.display()
        )))
    }
}

/// Unpack the regular files of `bytes` into `out`, refusing any entry whose
/// path would leave it.
fn unpack(bytes: &[u8], download: &ToolDownload, out: &Path) -> Result<()> {
    let unreadable = |e: &dyn std::fmt::Display| {
        Error::Operation(format!("unreadable archive {}: {e}", download.url))
    };
    match download.archive {
        Archive::Binary => {
            let name = match &download.setup {
                Some(Setup::Run { installer, .. }) => Path::new(installer),
                _ => Path::new(&download.executable),
            };
            write_entry(out, name, &mut &bytes[..])
        }
        Archive::TarGz => {
            let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
            for entry in archive.entries().map_err(|e| unreadable(&e))? {
                let mut entry = entry.map_err(|e| unreadable(&e))?;
                if entry.header().entry_type().is_symlink() {
                    let path = entry.path().map_err(|e| unreadable(&e))?.into_owned();
                    let target = entry
                        .link_name()
                        .map_err(|e| unreadable(&e))?
                        .ok_or_else(|| unreadable(&"a link without a target"))?
                        .into_owned();
                    write_link(out, &path, &target)?;
                    continue;
                }
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

/// Download `download`, check its sha256 and unpack it into a staging
/// directory beside `target`; returns the staging directory (removed when
/// dropped) and the unpacked tree holding the executable.
fn fetch_verified(
    download: &ToolDownload,
    target: &Path,
    timeout: u64,
    tools: &BTreeMap<String, String>,
) -> Result<(tempfile::TempDir, PathBuf)> {
    if let Some(Setup::UvVenv { lock, python }) = &download.setup {
        return uv_venv(download, target, timeout, tools, lock, python);
    }
    let executable = confined(Path::new(&download.executable), "executable")?;
    let bytes = fetch(&download.url, timeout)?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(&download.sha256) {
        return Err(Error::Operation(format!(
            "{} does not match its pinned sha256 (expected {}, got {actual}); nothing was installed",
            download.url, download.sha256
        )));
    }
    let parent = target.parent().expect("a version directory has a parent");
    fs::create_dir_all(parent)?;
    let staging = tempfile::tempdir_in(parent)?;
    let out = staging.path().join("out");
    fs::create_dir(&out)?;
    unpack(&bytes, download, &out)?;
    if let Some(Setup::Run {
        installer,
        args,
        env,
    }) = &download.setup
    {
        let installer = out.join(confined(Path::new(installer), "installer")?);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&installer, fs::Permissions::from_mode(0o755))?;
        }
        let args: Vec<String> = args.iter().map(|a| expand(a, &out)).collect();
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.clone(), expand(v, &out)))
            .collect();
        crate::process::run_env(&installer.to_string_lossy(), &args, &out, timeout, &env)
            .map_err(|e| Error::Operation(format!("installer {} failed: {e}", download.url)))?;
        fs::remove_file(&installer)?;
    }
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
    Ok((staging, out))
}

/// Build a Python tool's venv in a staging directory beside `target`.
fn uv_venv(
    download: &ToolDownload,
    target: &Path,
    timeout: u64,
    tools: &BTreeMap<String, String>,
    lock: &str,
    python: &str,
) -> Result<(tempfile::TempDir, PathBuf)> {
    let executable = confined(Path::new(&download.executable), "executable")?;
    let requirements = locked_requirements(lock)
        .ok_or_else(|| Error::Invalid(format!("no embedded requirements named {lock}")))?;
    let uv = tools.get("uv").ok_or_else(|| {
        Error::Invalid(format!(
            "installing {lock} needs uv: install it, pass --tool uv=PATH, or let depsmith init install uv first"
        ))
    })?;
    let parent = target.parent().expect("a version directory has a parent");
    fs::create_dir_all(parent)?;
    let staging = tempfile::tempdir_in(parent)?;
    let out = staging.path().join("out");
    fs::create_dir(&out)?;
    let venv: String = out.join("venv").to_string_lossy().into();
    let pinned = out.join("requirements.txt");
    fs::write(&pinned, requirements)?;
    let failed = |e: Error| Error::Operation(format!("installing {lock} with uv failed: {e}"));
    crate::process::run(
        uv,
        // The user's uv configuration must not pick the interpreter or index.
        &[
            "venv",
            "--relocatable",
            "--no-config",
            "--managed-python",
            "--python",
            python,
            &venv,
        ]
        .map(str::to_owned),
        &out,
        timeout,
    )
    .map_err(failed)?;
    crate::process::run(
        uv,
        &[
            "pip",
            "install",
            "--no-config",
            "--python",
            &venv,
            "--require-hashes",
            "--no-deps",
            "-r",
            &pinned.to_string_lossy(),
        ]
        .map(str::to_owned),
        &out,
        timeout,
    )
    .map_err(failed)?;
    if !out.join(&executable).is_file() {
        return Err(Error::Operation(format!(
            "installing {lock} produced no {}",
            executable.display()
        )));
    }
    Ok((staging, out))
}

/// Move the verified tree `out` to `target`, swapping out whatever is there.
fn place(out: &Path, target: &Path) -> Result<()> {
    if target.exists() {
        let parent = target.parent().expect("a version directory has a parent");
        let old = tempfile::Builder::new()
            .prefix(".replaced-")
            .tempdir_in(parent)?;
        fs::rename(target, old.path().join("tree"))?;
    }
    fs::rename(out, target)?;
    Ok(())
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
    install_with(name, version, download, cache, timeout, &BTreeMap::new())
}

/// [`install`], with the executables of companion tools a setup step needs
/// (`uv` for [`Setup::UvVenv`]).
///
/// # Errors
///
/// As for [`install`], plus a missing companion tool or a failed setup step.
pub fn install_with(
    name: &str,
    version: &str,
    download: &ToolDownload,
    cache: &Path,
    timeout: u64,
    tools: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    let executable = confined(Path::new(&download.executable), "executable")?;
    if let Some(path) = cached(name, version, download, cache) {
        return Ok(path);
    }
    let target = version_dir(name, version, cache)?;
    let (_staging, out) = fetch_verified(download, &target, timeout, tools)?;
    match place(&out, &target) {
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

/// A Python tool installed with uv from its embedded lock, on every host uv
/// supports.
fn python_tool(name: &str) -> Vec<ToolDownload> {
    [
        ("linux", "x86_64"),
        ("linux", "aarch64"),
        ("macos", "x86_64"),
        ("macos", "aarch64"),
        ("windows", "x86_64"),
        ("windows", "aarch64"),
    ]
    .iter()
    .map(|&(os, arch)| ToolDownload {
        os: os.into(),
        arch: arch.into(),
        executable: if os == "windows" {
            format!("venv/Scripts/{name}.exe")
        } else {
            format!("venv/bin/{name}")
        },
        setup: Some(Setup::UvVenv {
            lock: name.into(),
            python: "3.12".into(),
        }),
        ..Default::default()
    })
    .collect()
}

/// Node.js LTS by host, as (os, arch, Node platform, archive, sha256 from
/// the release's SHASUMS256.txt).
const NODE: &[(&str, &str, &str, Archive, &str)] = &[
    (
        "linux",
        "x86_64",
        "linux-x64",
        Archive::TarGz,
        "6e1db87ef58b8819e5d5402eff1536491b18edd8eb7bee5ef7897876e88dc5ff",
    ),
    (
        "linux",
        "aarch64",
        "linux-arm64",
        Archive::TarGz,
        "724282c3b43aec998aa9527380465b45d229e021b58035f5f4f63095eabfe5d5",
    ),
    (
        "macos",
        "x86_64",
        "darwin-x64",
        Archive::TarGz,
        "1462cb3b3046b815cf8ea436d3da450ec1a9f11dac7e5a46b0ada5305d7e8097",
    ),
    (
        "macos",
        "aarch64",
        "darwin-arm64",
        Archive::TarGz,
        "bed7eea5325e1108f32ce5228ddd6a5f0f08a499ee42aa7442aea583702f6057",
    ),
    (
        "windows",
        "x86_64",
        "win-x64",
        Archive::Zip,
        "158f7685b44de51f6c0df1d153526cbcd3e1bc739a8dfc607721cef75de9e541",
    ),
    (
        "windows",
        "aarch64",
        "win-arm64",
        Archive::Zip,
        "8779b1bde1d39f8d420e3b57aa657b39891af434d3de44a919044cec06785921",
    ),
];
/// The Node.js release whose bundled npm depsmith installs (and CI pins in
/// `.github/tool-versions.json`).
pub const NODE_VERSION: &str = "24.21.0";

/// npm as bundled with a pinned Node.js LTS; Node's directory is first on
/// `PATH` so npm runs on that Node.
fn node_downloads() -> Vec<ToolDownload> {
    NODE.iter()
        .map(|&(os, arch, platform, archive, sha256)| {
            let root = format!("node-v{NODE_VERSION}-{platform}");
            let (url, executable, bin) = if os == "windows" {
                ("zip", format!("{root}/npm.cmd"), root.clone())
            } else {
                ("tar.gz", format!("{root}/bin/npm"), format!("{root}/bin"))
            };
            ToolDownload {
                os: os.into(),
                arch: arch.into(),
                url: format!("https://nodejs.org/dist/v{NODE_VERSION}/{root}.{url}"),
                sha256: sha256.into(),
                archive,
                executable,
                path: vec![bin],
                ..Default::default()
            }
        })
        .collect()
}

/// rustup-init, by host, as (os, arch, target triple, sha256).
const RUSTUP: &[(&str, &str, &str, &str)] = &[
    (
        "linux",
        "x86_64",
        "x86_64-unknown-linux-gnu",
        "dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71",
    ),
    (
        "linux",
        "aarch64",
        "aarch64-unknown-linux-gnu",
        "15f6e4ce9f583b929c996c91562bad6d4454f3281de858b02cdfdef615fac433",
    ),
    (
        "macos",
        "x86_64",
        "x86_64-apple-darwin",
        "259e2b84274434085163fe8d556510571772cda2aa6d87ca6aa664f57bc644e3",
    ),
    (
        "macos",
        "aarch64",
        "aarch64-apple-darwin",
        "ec1b9233e7f72990ecd8e62063fa7f6c3dfc2bec8e97f88bff165f9100ac696a",
    ),
    (
        "windows",
        "x86_64",
        "x86_64-pc-windows-msvc",
        "6f4bef66261261fcb43131be8720bab817d403a09edec7455c371974b90bdb7e",
    ),
    (
        "windows",
        "aarch64",
        "aarch64-pc-windows-msvc",
        "01aa49cf9574a8bd0ae52005d7de2590e8f27181ded6748236e702c92aef826d",
    ),
];
const RUSTUP_VERSION: &str = "1.29.1";
/// The toolchain `depsmith init` installs for cargo: its tested version.
const RUST_TOOLCHAIN: &str = "1.98.1";

/// cargo through a pinned rustup-init, which installs the pinned toolchain
/// (minimal profile, verified by rustup) into the install directory without
/// touching the user's PATH or shell profile.
fn rustup_downloads() -> Vec<ToolDownload> {
    let homes = |home: &str| {
        [
            ("RUSTUP_HOME".to_owned(), format!("{{prefix}}/{home}rustup")),
            ("CARGO_HOME".to_owned(), format!("{{prefix}}/{home}cargo")),
        ]
    };
    RUSTUP
        .iter()
        .map(|&(os, arch, triple, sha256)| {
            let exe = if os == "windows" { ".exe" } else { "" };
            let mut setup_env: BTreeMap<String, String> = homes("").into_iter().collect();
            setup_env.insert("RUSTUP_INIT_SKIP_PATH_CHECK".into(), "yes".into());
            // At run time only the toolchain comes from the install: cargo
            // keeps the user's CARGO_HOME (config, credentials, registry).
            // The cargo adapter drops RUSTUP_TOOLCHAIN for projects with a
            // rust-toolchain file.
            let mut env = BTreeMap::from([(
                "RUSTUP_HOME".to_owned(),
                "{prefix}/rustup".to_owned(),
            )]);
            env.insert("RUSTUP_TOOLCHAIN".into(), RUST_TOOLCHAIN.into());
            ToolDownload {
                os: os.into(),
                arch: arch.into(),
                url: format!(
                    "https://static.rust-lang.org/rustup/archive/{RUSTUP_VERSION}/{triple}/rustup-init{exe}"
                ),
                sha256: sha256.into(),
                archive: Archive::Binary,
                executable: format!("cargo/bin/cargo{exe}"),
                setup: Some(Setup::Run {
                    installer: format!("rustup-init{exe}"),
                    args: [
                        "-y",
                        "--no-modify-path",
                        "--profile",
                        "minimal",
                        "--default-toolchain",
                        RUST_TOOLCHAIN,
                    ]
                    .map(str::to_owned)
                    .into(),
                    env: setup_env,
                }),
                env,
                path: vec!["cargo/bin".into()],
            }
        })
        .collect()
}

/// The pinned downloads of the tool named `name` (its first tested
/// version); empty for tools depsmith does not install.
pub fn pinned(name: &str) -> Vec<ToolDownload> {
    if name == "cargo" {
        return rustup_downloads();
    }
    if name == "conda-lock" {
        return python_tool("conda-lock");
    }
    if name == "npm" {
        return node_downloads();
    }
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
                ..Default::default()
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
    /// Manifest hash of the whole install directory (see [`tree_hash`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tree: Option<String>,
}

/// Names a running tool may create or update inside its install (Python
/// bytecode caches, rustup's download and temporary directories), left out
/// of manifests.
const VOLATILE: &[&str] = &["__pycache__", "downloads", "tmp", "update-hash"];

/// A manifest hash of the files and links under `dir`, sorted by path,
/// leaving out [`VOLATILE`] entries and `.pyc` files.
fn tree_hash(dir: &Path) -> Result<String> {
    fn walk(dir: &Path, relative: &Path, lines: &mut Vec<String>) -> Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name();
            let lossy = name.to_string_lossy();
            if VOLATILE.contains(&lossy.as_ref()) || lossy.ends_with(".pyc") {
                continue;
            }
            let path = relative.join(&name);
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                let target = fs::read_link(entry.path())?;
                lines.push(format!("L {} {}", path.display(), target.display()));
            } else if kind.is_dir() {
                walk(&entry.path(), &path, lines)?;
            } else {
                lines.push(format!(
                    "F {} {}",
                    path.display(),
                    file_sha256(&entry.path())?
                ));
            }
        }
        Ok(())
    }
    let mut lines = vec![];
    walk(dir, Path::new(""), &mut lines)?;
    Ok(format!("{:x}", Sha256::digest(lines.join("\n"))))
}

/// The install directory of `download` whose executable is `executable`.
fn install_dir<'a>(download: &ToolDownload, executable: &'a Path) -> Option<&'a Path> {
    executable
        .ancestors()
        .nth(Path::new(&download.executable).components().count())
}

/// Whether the install of `tool` recorded for `root` still has the tree it
/// was installed with; the reason when it does not. Slower than [`trust`]
/// (it hashes the whole install), so `depsmith init` runs it, not every run.
pub fn verify_tree(root: &Path, tool: &ToolSpec) -> Option<String> {
    let record = read_records(root).remove(&tool.name)?;
    let expected = record.tree?;
    let dir = install_dir(host_download(tool)?, &record.executable)?;
    match tree_hash(dir) {
        Ok(actual) if actual == expected => None,
        Ok(_) => Some(format!(
            "the installed {} in {} changed since depsmith installed it",
            tool.name,
            dir.display()
        )),
        Err(_) => Some(format!(
            "the installed {} in {} is gone",
            tool.name,
            dir.display()
        )),
    }
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
    let version = tool.pinned_version().unwrap_or("");
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
    crate::transaction::safe_path(root, Path::new(RECORDS))?;
    // Concurrent runs in this repository must not lose each other's records.
    let _lock = crate::transaction::operation_lock(root)?;
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
            version: tool.pinned_version().unwrap_or_default().to_owned(),
            executable: executable.to_path_buf(),
            sha256: file_sha256(executable)?,
            tree: host_download(tool)
                .and_then(|d| install_dir(d, executable))
                .map(tree_hash)
                .transpose()?,
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

/// Download `tool` afresh and record it in `root`. A cached copy, which
/// cannot be verified once unpacked, is compared with the fresh download:
/// kept when identical (other repositories may be running it), otherwise
/// replaced.
///
/// # Errors
///
/// As for [`install`], plus failures to replace the old copy or to write the
/// record (including a symlinked `.depsmith`).
pub fn reinstall(root: &Path, tool: &ToolSpec, cache: &Path, timeout: u64) -> Result<PathBuf> {
    reinstall_with(root, tool, cache, timeout, &BTreeMap::new())
}

/// [`reinstall`], with the executables of companion tools a setup step
/// needs (see [`install_with`]).
///
/// # Errors
///
/// As for [`reinstall`] and [`install_with`].
pub fn reinstall_with(
    root: &Path,
    tool: &ToolSpec,
    cache: &Path,
    timeout: u64,
    tools: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    crate::transaction::safe_path(root, Path::new(RECORDS))?;
    let download = host_download(tool).ok_or_else(|| {
        Error::Invalid(format!(
            "{} has no download for {}-{}",
            tool.name,
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    })?;
    let version = tool
        .pinned_version()
        .ok_or_else(|| Error::Invalid(format!("{} has no tested version to install", tool.name)))?;
    let executable = confined(Path::new(&download.executable), "executable")?;
    let target = version_dir(&tool.name, version, cache)?;
    let (_staging, out) = fetch_verified(download, &target, timeout, tools)?;
    // Keep a cached copy only if its whole tree equals the fresh install
    // (other repositories may be running it); a broken one is replaced.
    let identical = target.exists()
        && matches!((tree_hash(&target), tree_hash(&out)), (Ok(old), Ok(new)) if old == new);
    if !identical {
        place(&out, &target)?;
    }
    let path = target.join(executable);
    record(root, tool, &path)?;
    Ok(path)
}

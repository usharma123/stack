//! `stack setup`: make the machine ready without installing anything by hand. Downloads the mise
//! release stack is tested against into stack's own data directory, verified by its SHA-256.

use crate::error::{io_error, Result, StackError};
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The release CI runs the service scenarios against (see `.github/workflows/validate.yml`).
pub const MISE_VERSION: &str = "2026.9.18";

const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;

/// Release asset and its SHA-256 for this platform. Linux uses the static musl build, like stack.
fn asset() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(("macos-arm64", "b3539de1a9823505269481b71d09a9cf86e141c63ebfc6101b3cf561766a83e8")),
        ("macos", "x86_64") => Some(("macos-x64", "e1fd0d0a7c93428cc4eaf61a0bdb26f0a50d4401283458c1182d3dcc6fb9b234")),
        ("linux", "aarch64") => Some(("linux-arm64-musl", "581eff012396884044ba97b9e30c446492d2ee40bbf78388eecc0fc17973856d")),
        ("linux", "x86_64") => Some(("linux-x64-musl", "f7530faed716ec24457609c0278618770a1b4b8f856b61686ac685c148e3ffd1")),
        _ => None,
    }
}

/// Where `stack setup` puts binaries: `$STACK_DATA_DIR/bin`, else the XDG data directory.
pub fn bin_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("STACK_DATA_DIR") {
        return PathBuf::from(dir).join("bin");
    }
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(dir).join("stack/bin");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/share/stack/bin")
}

/// Puts the managed mise last on PATH, so every provider call, nested `mise` command and
/// `stack exec` child finds it, while a mise the user installed still wins. Call before any
/// thread starts.
pub fn use_managed_tools() {
    let dir = bin_dir();
    if !dir.join("mise").is_file() {
        return;
    }
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if paths.contains(&dir) {
        return;
    }
    paths.push(dir);
    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var("PATH", joined);
    }
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub mise: PathBuf,
    pub version: String,
    /// False when a usable mise was already on PATH.
    pub installed: bool,
}

/// Leaves a working mise alone unless `force`; otherwise installs the pinned release.
pub fn run(force: bool) -> Result<Report> {
    if !force {
        if let Some((path, version)) = find_on_path().and_then(|p| version(&p).map(|v| (p, v))) {
            return Ok(Report { mise: path, version, installed: false });
        }
    }
    let (platform, sha256) = asset().ok_or_else(|| {
        StackError::new("setup_unsupported", format!("no mise build for {}-{}", std::env::consts::OS, std::env::consts::ARCH))
            .hint(MISE_INSTALL_HINT)
    })?;
    let url = format!("https://github.com/jdx/mise/releases/download/v{MISE_VERSION}/mise-v{MISE_VERSION}-{platform}.tar.gz");
    let archive = download(&url)?;
    let path = install(&archive, sha256, &bin_dir())?;
    let version = version(&path).ok_or_else(|| {
        StackError::new("setup_failed", format!("{} was installed but does not run", path.display())).hint(MISE_INSTALL_HINT)
    })?;
    Ok(Report { mise: path, version, installed: true })
}

pub const MISE_INSTALL_HINT: &str = "run `stack setup` to download mise, or install it yourself: https://mise.jdx.dev";

/// The mise a bare `mise` command runs: the first executable on PATH.
fn find_on_path() -> Option<PathBuf> {
    find_on_path_from(Path::new(""))
}

/// The mise a bare `mise` command run in `dir` runs. A relative PATH entry, including the
/// empty one that means the working directory, is found under `dir`, as that command finds it.
pub fn find_on_path_from(dir: &Path) -> Option<PathBuf> {
    find_in(&std::env::var_os("PATH")?, dir)
}

/// The first file named `mise` in `paths` this process may run, as the OS searches them. A
/// directory named `mise` is skipped even with its search bits set, and so is a file whose
/// execute bits are all for someone else, as the OS skips both.
fn find_in(paths: &std::ffi::OsStr, dir: &Path) -> Option<PathBuf> {
    std::env::split_paths(paths)
        .map(|d| dir.join(d).join("mise"))
        .find(|p| p.is_file() && may_execute(p))
}

/// Whether `execve` would let this process run `path`: the kernel's own check, for this
/// process's user and groups and any ACL, not the mode bits alone.
#[cfg(unix)]
fn may_execute(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: the path is NUL-terminated and only read; the id calls cannot fail.
    unsafe {
        // `access` checks the real ids, `execve` the effective ones. They are the same unless
        // stack runs set-id, and plain `access` works where a container's seccomp profile
        // refuses the `faccessat2` that `AT_EACCESS` needs on Linux.
        if libc::getuid() == libc::geteuid() && libc::getgid() == libc::getegid() {
            libc::access(path.as_ptr(), libc::X_OK) == 0
        } else {
            libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0
        }
    }
}

#[cfg(not(unix))]
fn may_execute(path: &Path) -> bool {
    crate::hash::is_executable(path)
}

fn version(mise: &Path) -> Option<String> {
    let out = Command::new(mise).arg("--version").stdin(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().trim().to_string())
}

fn download(url: &str) -> Result<Vec<u8>> {
    let fail = |why: String| {
        StackError::new("setup_failed", format!("cannot download {url}: {why}")).hint(MISE_INSTALL_HINT)
    };
    let resp = ureq::AgentBuilder::new()
        .timeout(DOWNLOAD_TIMEOUT)
        .try_proxy_from_env(true)
        .build()
        .get(url)
        .call()
        .map_err(|e| fail(e.to_string()))?;
    let mut buf = Vec::new();
    resp.into_reader().take(MAX_ARCHIVE + 1).read_to_end(&mut buf).map_err(|e| fail(e.to_string()))?;
    if buf.len() as u64 > MAX_ARCHIVE {
        return Err(fail("archive exceeds 256 MiB".into()));
    }
    Ok(buf)
}

/// Checks the archive against `sha256`, then atomically places its `mise/bin/mise` in `dir`.
pub fn install(archive: &[u8], sha256: &str, dir: &Path) -> Result<PathBuf> {
    let actual = crate::hash::sha256_hex(archive);
    if actual != sha256 {
        return Err(StackError::new("setup_failed", format!("mise archive has SHA-256 {actual}, expected {sha256}")));
    }
    std::fs::create_dir_all(dir).map_err(|e| io_error(dir.display(), e))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let entries = tar.entries().map_err(|e| io_error("mise archive", e))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| io_error("mise archive", e))?;
        let is_binary = entry.path().map(|p| p == Path::new("mise/bin/mise")).unwrap_or(false);
        if !is_binary || !entry.header().entry_type().is_file() {
            continue;
        }
        let mut staged = tempfile::NamedTempFile::new_in(dir).map_err(|e| io_error(dir.display(), e))?;
        std::io::copy(&mut entry, &mut staged).map_err(|e| io_error("mise archive", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(0o755))
                .map_err(|e| io_error(staged.path().display(), e))?;
        }
        let target = dir.join("mise");
        staged.persist(&target).map_err(|e| io_error(target.display(), e.error))?;
        return Ok(target);
    }
    Err(StackError::new("setup_failed", "mise archive does not contain mise/bin/mise"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(path: &str, body: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, body).unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn finds_the_first_executable_file_as_the_os_does() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = |name: &str| {
            let path = root.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            path
        };
        let executable = |path: PathBuf, mode: u32| {
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        };
        let searchable = std::fs::Permissions::from_mode(0o755);
        std::fs::create_dir(dir("folder").join("mise")).unwrap();
        std::fs::set_permissions(root.path().join("folder/mise"), searchable).unwrap();
        executable(dir("plain").join("mise"), 0o644);
        // Executable by others but not by its owner, this process: the OS passes it by.
        let theirs = executable(dir("theirs").join("mise"), 0o645);
        std::os::unix::fs::symlink(root.path().join("folder/mise"), dir("to-folder").join("mise")).unwrap();
        let real = executable(dir("real").join("mise"), 0o755);
        std::os::unix::fs::symlink(&real, dir("link").join("mise")).unwrap();
        let path = |entries: &[&str]| std::env::join_paths(entries.iter().map(|e| root.path().join(e))).unwrap();

        let skipped = path(&["folder", "plain", "to-folder", "real"]);
        assert_eq!(find_in(&skipped, Path::new("")), Some(real.clone()));
        let linked = path(&["folder", "link", "real"]);
        assert_eq!(find_in(&linked, Path::new("")), Some(root.path().join("link/mise")));
        assert_eq!(find_in(&path(&["folder", "plain"]), Path::new("")), None);
        // Root may run any file with an execute bit, so for root the OS runs `theirs`.
        // SAFETY: geteuid cannot fail.
        let runs_theirs = unsafe { libc::geteuid() } == 0;
        let first = if runs_theirs { theirs } else { real.clone() };
        assert_eq!(find_in(&path(&["theirs", "real"]), Path::new("")), Some(first.clone()));
        // Relative entries, the empty one included, are found under `dir`.
        let relative = std::env::join_paths(["folder", "", "theirs", "real"]).unwrap();
        assert_eq!(find_in(&relative, root.path()), Some(first));
    }

    #[test]
    fn installs_the_binary_from_a_verified_archive() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = archive("mise/bin/mise", b"#!/bin/sh\n");
        let path = install(&bytes, &crate::hash::sha256_hex(&bytes), dir.path()).unwrap();
        assert_eq!(path, dir.path().join("mise"));
        assert_eq!(std::fs::read(&path).unwrap(), b"#!/bin/sh\n");
        assert!(crate::hash::is_executable(&path));
    }

    #[test]
    fn rejects_a_mismatched_hash_or_a_missing_binary() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = archive("mise/bin/mise", b"x");
        let err = install(&bytes, &"0".repeat(64), dir.path()).unwrap_err();
        assert!(err.to_string().contains("expected"), "{err}");
        assert!(!dir.path().join("mise").exists());

        let bytes = archive("mise/README.md", b"x");
        let err = install(&bytes, &crate::hash::sha256_hex(&bytes), dir.path()).unwrap_err();
        assert!(err.to_string().contains("does not contain"), "{err}");
    }

    #[test]
    fn every_shipped_platform_has_a_pinned_build() {
        assert!(asset().is_some(), "stack ships for this platform, so setup must too");
    }
}

use crate::error::{io_error, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Content hash of a bundle tree: relative paths, executable bits and file contents.
pub fn hash_dir(root: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();

    let mut hasher = Sha256::new();
    for rel in files {
        let path = root.join(&rel);
        let contents = fs::read(&path).map_err(|e| io_error(path.display(), e))?;
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update([0, is_executable(&path) as u8]);
        hasher.update((contents.len() as u64).to_le_bytes());
        hasher.update(&contents);
    }
    Ok(format!("sha256:{}", hex(&hasher.finalize())))
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(|e| io_error(dir.display(), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_error(dir.display(), e))?;
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" || name == ".stack" {
            continue;
        }
        let file_type = entry.file_type().map_err(|e| io_error(path.display(), e))?;
        if file_type.is_dir() {
            collect_files(root, &path, out)?;
        } else if file_type.is_file() {
            out.push(path.strip_prefix(root).expect("path under root").to_path_buf());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

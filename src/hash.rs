use crate::error::{io_error, Result, StackError};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Directory names that are never bundle content, at any depth: not hashed, not archived,
/// refused in OCI layers, and never read at compose time (see `paths.bin`).
pub const EXCLUDED: [&str; 2] = [".git", ".stack"];

/// Whether `name` is one of [`EXCLUDED`], however a case-insensitive filesystem (macOS) would
/// spell it: `.Git` there is the repository Git uses, and `.STACK` the directory Stack writes
/// its state into, so case is folded (with the letters that fold to these, such as `ſ` to `s`)
/// and the code points HFS+ ignores in names are dropped, as Git itself does when it protects
/// `.git`. Folded on every platform, so a tree is the same bundle content wherever it is hashed.
pub fn excluded(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        let folded: String = name
            .chars()
            .filter(|c| !matches!(c, '\u{200C}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{206A}'..='\u{206F}' | '\u{FEFF}'))
            .flat_map(|c| if c == '\u{17F}' { 's' } else { c }.to_lowercase())
            .collect();
        EXCLUDED.contains(&folded.as_str())
    })
}

/// The first entry under `root`, at any depth, whose name is one of [`EXCLUDED`] (see
/// [`excluded`]), relative to `root`. A tree expanded from an OCI layer has none (they are
/// refused there), so one found in a cached tree means it was filled some other way (by an
/// earlier Stack, which compared names exactly), with content its hash does not cover.
pub fn first_excluded(root: &Path) -> Result<Option<PathBuf>> {
    fn walk(root: &Path, dir: &Path) -> Result<Option<PathBuf>> {
        for entry in fs::read_dir(dir).map_err(|e| io_error(dir.display(), e))? {
            let entry = entry.map_err(|e| io_error(dir.display(), e))?;
            let path = entry.path();
            if excluded(&entry.file_name()) {
                return Ok(Some(path.strip_prefix(root).expect("path under root").to_path_buf()));
            }
            if entry.file_type().map_err(|e| io_error(path.display(), e))?.is_dir() {
                if let Some(found) = walk(root, &path)? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }
    walk(root, root)
}

/// Content hash of a bundle tree: relative paths, executable bits and file contents.
///
/// A bundle is regular files and directories only. A symbolic link (or socket, FIFO or device)
/// anywhere in the tree is `bundle_invalid` rather than skipped: skipped, its target would be
/// readable through the bundle without being covered by the hash. Paths must be UTF-8 so that
/// distinct names never hash alike. Git checkouts, local directories and OCI layers all pass
/// through here, so every transport holds bundles to the same rule.
pub fn hash_dir(root: &Path) -> Result<String> {
    Ok(hash_keeping(root, "")?.0)
}

/// [`hash_dir`], and the bytes of the file at `keep` (relative to `root`) exactly as they were
/// hashed, when the tree has one: what is parsed from it is then what the hash covers, however
/// the file changes afterwards.
pub fn hash_keeping(root: &Path, keep: &str) -> Result<(String, Option<Vec<u8>>)> {
    let files = list_files(root)?;
    let mut hasher = Sha256::new();
    let mut kept = None;
    for rel in files {
        let path = root.join(&rel);
        let contents = fs::read(&path).map_err(|e| io_error(path.display(), e))?;
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update([0, is_executable(&path) as u8]);
        hasher.update((contents.len() as u64).to_le_bytes());
        hasher.update(&contents);
        if !keep.is_empty() && rel == Path::new(keep) {
            kept = Some(contents);
        }
    }
    Ok((format!("sha256:{}", hex(&hasher.finalize())), kept))
}

/// Files under `root` (skipping [`EXCLUDED`] directories), relative and sorted. Anything but a
/// regular file or directory, and any name that is not UTF-8, is `bundle_invalid` (see
/// [`hash_dir`]).
pub fn list_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(|e| io_error(dir.display(), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| io_error(dir.display(), e))?;
        let path = entry.path();
        let name = entry.file_name();
        let rel = path.strip_prefix(root).expect("path under root").to_path_buf();
        // Checked before the exclusions, so a non-UTF-8 name cannot pose as one.
        if name.to_str().is_none() {
            return Err(StackError::new(
                "bundle_invalid",
                format!("{}: bundle path {} is not valid UTF-8", root.display(), rel.display()),
            )
            .hint("rename the file; bundle paths must be UTF-8"));
        }
        // Whatever their type: a worktree's or submodule's `.git` is a file. Never read through
        // a bundle, so never hashed.
        if excluded(&name) {
            continue;
        }
        let file_type = entry.file_type().map_err(|e| io_error(path.display(), e))?;
        if file_type.is_dir() {
            collect_files(root, &path, out)?;
        } else if file_type.is_file() {
            out.push(rel);
        } else {
            let what = if file_type.is_symlink() { "a symbolic link" } else { "not a regular file or directory" };
            return Err(StackError::new(
                "bundle_invalid",
                format!("{}: {} is {what}; bundles contain only regular files and directories", root.display(), rel.display()),
            )
            .hint("replace it with a copy of the file or directory it stands for, then retry"));
        }
    }
    Ok(())
}

#[cfg(unix)]
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub fn is_executable(_path: &Path) -> bool {
    false
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("bin")).unwrap();
        fs::write(dir.path().join("bundle.toml"), "[bundle]\nname = 'b'\n").unwrap();
        fs::write(dir.path().join("bin/tool"), "#!/bin/sh\n").unwrap();
        dir
    }

    #[test]
    fn a_tree_of_regular_files_hashes_as_before() {
        // Pinned (path, NUL, executable bit, length, contents per file, sorted): every
        // stack.lock records hashes computed this way, so legitimate trees must not change.
        let dir = tree();
        let hash = hash_dir(dir.path()).unwrap();
        assert_eq!(hash, "sha256:791895fe3cb2f9c3a1ec36911896904511a4d63a32999f99ea9e2287e835bc12");
        // Excluded directories and a worktree's `.git` file are not content, whatever they hold.
        fs::create_dir_all(dir.path().join(".stack/state")).unwrap();
        fs::write(dir.path().join(".stack/state/x"), "x").unwrap();
        fs::write(dir.path().join("bin/.git"), "gitdir: elsewhere\n").unwrap();
        symlink("/etc", dir.path().join(".stack/link")).unwrap();
        assert_eq!(hash_dir(dir.path()).unwrap(), hash);
        // Nor are they spelled another way, which on a case-insensitive filesystem is the same
        // directory: Git's repository there, or the state Stack writes.
        fs::create_dir_all(dir.path().join("bin/.Stack")).unwrap();
        fs::write(dir.path().join("bin/.Stack/x"), "x").unwrap();
        if fs::create_dir(dir.path().join(".GIT")).is_ok() {
            fs::write(dir.path().join(".GIT/config"), "[remote]\n").unwrap();
        }
        assert_eq!(hash_dir(dir.path()).unwrap(), hash);
        // The file kept is the one hashed, byte for byte.
        let (again, kept) = hash_keeping(dir.path(), "bundle.toml").unwrap();
        assert_eq!((again, kept.as_deref()), (hash.clone(), Some(&b"[bundle]\nname = 'b'\n"[..])));
        assert_eq!(hash_keeping(dir.path(), "missing.toml").unwrap().1, None);
        assert!(first_excluded(dir.path()).unwrap().is_some());
    }

    #[test]
    fn excluded_names_are_folded_as_a_case_insensitive_filesystem_folds_them() {
        use std::ffi::OsStr;
        for name in [".git", ".stack", ".GIT", ".Git", ".sTaCk", ".\u{17F}tack", ".g\u{200C}it", "\u{FEFF}.git", ".STAC\u{212A}"] {
            assert!(excluded(OsStr::new(name)), "{name:?}");
        }
        for name in [".gitignore", ".github", "git", ".stackrc", ".gi", "stack", ".g it"] {
            assert!(!excluded(OsStr::new(name)), "{name:?}");
        }
        let dir = tree();
        assert_eq!(first_excluded(dir.path()).unwrap(), None);
        fs::create_dir_all(dir.path().join("bin/deep/.Stack")).unwrap();
        assert_eq!(first_excluded(dir.path()).unwrap(), Some(PathBuf::from("bin/deep/.Stack")));
    }

    #[test]
    fn links_anywhere_in_a_bundle_are_refused_rather_than_skipped() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "unhashed").unwrap();
        for (name, target) in [
            ("bin/escape", outside.path().join("secret")),
            ("bin/dir", outside.path().to_path_buf()),
            ("bin/inside", std::path::PathBuf::from("tool")),
            ("dangling", std::path::PathBuf::from("/nonexistent/x")),
            ("bundle.toml.link", std::path::PathBuf::from("bundle.toml")),
        ] {
            let dir = tree();
            symlink(&target, dir.path().join(name)).unwrap();
            for result in [hash_dir(dir.path()).map(drop), list_files(dir.path()).map(drop)] {
                let err = result.unwrap_err();
                assert_eq!(err.code, "bundle_invalid", "{name}");
                assert!(err.message.contains(&format!("{name} is a symbolic link")), "{name}: {}", err.message);
            }
        }
    }

    #[test]
    fn special_files_are_refused() {
        let dir = tree();
        let fifo = dir.path().join("bin/fifo");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status().is_ok_and(|s| s.success());
        if !made {
            return;
        }
        let err = hash_dir(dir.path()).unwrap_err();
        assert_eq!(err.code, "bundle_invalid");
        assert!(err.message.contains("not a regular file or directory"), "{}", err.message);
    }

    #[test]
    fn names_that_are_not_utf8_are_refused() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tree();
        // Some file systems (APFS) refuse such names outright; nothing to check there.
        if fs::write(dir.path().join(std::ffi::OsStr::from_bytes(b"bin/\xff")), "x").is_err() {
            return;
        }
        let err = hash_dir(dir.path()).unwrap_err();
        assert_eq!(err.code, "bundle_invalid");
        assert!(err.message.contains("not valid UTF-8"), "{}", err.message);
    }
}

//! Keeping the files stack generates in a checkout out of version control.
//!
//! Stack lists them in the repository's `info/exclude`: local to the clone, never committed,
//! and read by every worktree. Each checkout gets one block of exact paths, keyed by its
//! absolute directory and rewritten whole when what it generates changes: the provider config
//! (`.config/mise/conf.d/stack.toml`), the rendered lock (`.config/mise/mise.lock`) and mise's
//! `locks/` beside it, `.stack/`, and the skill links stack recorded with their registry. Lines
//! outside stack's blocks are never changed, and a block whose checkout no longer exists is
//! dropped. Nothing else under `.config` or the skills directory is ignored, and a file git
//! already tracks stays tracked.

use std::path::{Path, PathBuf};
use std::process::Command;

const BEGIN: &str = "# stack: generated files of ";
const END: &str = "# stack: end of ";

/// Bring this checkout's block up to date. Outside a git repository, or where git cannot say
/// where its exclude file is, nothing happens. Returns a warning when the file cannot be
/// written; generating files never fails because of it.
pub fn update(root: &Path, skills_dir: Option<&str>) -> Option<String> {
    let (exclude, prefix) = locate(root)?;
    if root.to_string_lossy().contains('\n') {
        return None;
    }
    // A directory stack would refuse to link into holds nothing of stack's.
    let skills_dir = skills_dir.filter(|dir| crate::skills::validate_dir(dir).is_ok());
    let links = skills_dir.map(|dir| crate::skills::linked(&root.join(dir))).unwrap_or_default();
    let entries = entries(&prefix, skills_dir.map(|dir| (dir, links.as_slice())));
    let current = match std::fs::read_to_string(&exclude) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Some(format!("cannot read {}: {e}; stack's generated files are not excluded from git", exclude.display())),
    };
    let next = rewrite(&current, root, &entries, |dir| dir.is_dir());
    if next == current {
        return None;
    }
    write(&exclude, &next).err().map(|e| format!("cannot write {}: {e}; stack's generated files are not excluded from git", exclude.display()))
}

/// The exclude file git reads for `root`'s repository, and `root` relative to its work tree
/// (`""` at the top, else ending in `/`).
fn locate(root: &Path) -> Option<(PathBuf, String)> {
    let mut command = Command::new("git");
    command.arg("-C").arg(root).args(["rev-parse", "--git-path", "info/exclude", "--show-prefix"]).env("GIT_TERMINAL_PROMPT", "0");
    let out = crate::process::output(&mut command).ok().filter(|o| o.status.success())?;
    let text = String::from_utf8(out.stdout).ok()?;
    let mut lines = text.lines();
    let exclude = root.join(lines.next()?);
    let prefix = lines.next().unwrap_or_default().to_string();
    (!prefix.contains('\n')).then_some((exclude, prefix))
}

/// The patterns of one checkout, anchored at the work tree's top.
fn entries(prefix: &str, skills: Option<(&str, &[String])>) -> Vec<String> {
    let at = |path: &str| format!("/{}", escape(&format!("{prefix}{path}")));
    let mut out = vec![at(".stack/"), at(".config/mise/conf.d/stack.toml"), at(".config/mise/mise.lock"), at(".config/mise/locks/")];
    if let Some((dir, links)) = skills {
        let dir = dir.trim_end_matches('/');
        out.push(at(&format!("{dir}/{}", crate::skills::REGISTRY)));
        out.extend(links.iter().map(|name| at(&format!("{dir}/{name}"))));
    }
    out
}

/// A path as a gitignore pattern that matches it literally.
fn escape(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    // Trailing spaces are dropped unless escaped.
    let kept = out.trim_end_matches(' ').len();
    let spaces = out.len() - kept;
    out.truncate(kept);
    out.push_str(&"\\ ".repeat(spaces));
    out
}

/// `text` with `root`'s block replaced by `entries` (appended when it has none), and blocks of
/// checkouts `exists` no longer finds removed. Everything else is kept as it was.
fn rewrite(text: &str, root: &Path, entries: &[String], exists: impl Fn(&Path) -> bool) -> String {
    let own = root.display().to_string();
    let mut kept: Vec<&str> = Vec::new();
    let mut skipping: Option<String> = None;
    for line in text.lines() {
        if let Some(block) = &skipping {
            if line == format!("{END}{block}") {
                skipping = None;
            }
            continue;
        }
        if let Some(block) = line.strip_prefix(BEGIN) {
            if block == own || !exists(Path::new(block)) {
                skipping = Some(block.to_string());
                continue;
            }
        }
        kept.push(line);
    }
    while kept.last().is_some_and(|l| l.is_empty()) {
        kept.pop();
    }
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&format!("{BEGIN}{own}\n"));
    for entry in entries {
        out.push_str(entry);
        out.push('\n');
    }
    out.push_str(&format!("{END}{own}\n"));
    out
}

/// Replace `path` whole, so a reader never sees half a file.
fn write(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp, text.as_bytes())?;
    if let Ok(meta) = std::fs::metadata(path) {
        std::fs::set_permissions(tmp.path(), meta.permissions())?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checkouts_block_is_rewritten_whole_and_other_lines_are_kept() {
        let root = Path::new("/w/app");
        let user = "# git ls-files --others --exclude-from=.git/info/exclude\n*.log\n";
        let first = rewrite(user, root, &["/app/.stack/".into()], |_| true);
        assert_eq!(first, format!("{user}\n{BEGIN}/w/app\n/app/.stack/\n{END}/w/app\n"));
        let again = rewrite(&first, root, &["/app/.stack/".into(), "/app/x".into()], |_| true);
        assert_eq!(again, format!("{user}\n{BEGIN}/w/app\n/app/.stack/\n/app/x\n{END}/w/app\n"));
        assert_eq!(rewrite(&again, root, &["/app/.stack/".into(), "/app/x".into()], |_| true), again, "unchanged");
    }

    #[test]
    fn blocks_of_other_checkouts_stay_until_their_directory_is_gone() {
        let other = format!("{BEGIN}/w/other\n/app/.stack/\n{END}/w/other\n");
        let text = rewrite(&other, Path::new("/w/app"), &["/app/.stack/".into()], |_| true);
        assert!(text.starts_with(&other), "{text}");
        let text = rewrite(&text, Path::new("/w/app"), &["/app/.stack/".into()], |p| p != Path::new("/w/other"));
        assert_eq!(text, format!("{BEGIN}/w/app\n/app/.stack/\n{END}/w/app\n"));
    }

    #[test]
    fn entries_name_exact_generated_paths_and_recorded_links_only() {
        let links = ["jq".to_string(), "fnox".to_string()];
        assert_eq!(
            entries("sub dir/", Some((".claude/skills/", &links))),
            [
                "/sub dir/.stack/",
                "/sub dir/.config/mise/conf.d/stack.toml",
                "/sub dir/.config/mise/mise.lock",
                "/sub dir/.config/mise/locks/",
                "/sub dir/.claude/skills/.stack-skills.json",
                "/sub dir/.claude/skills/jq",
                "/sub dir/.claude/skills/fnox",
            ]
        );
        assert_eq!(escape("a*b?[c]\\ "), "a\\*b\\?\\[c\\]\\\\\\ ");
    }
}

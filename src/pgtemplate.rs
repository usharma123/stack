//! Initialised Postgres clusters kept as templates, so a new data directory is a copy-on-write
//! clone of one instead of a fresh `initdb` (half a second of CPU and disk each, and six at once
//! contend for both).
//!
//! A template is what `initdb -U postgres --auth=trust` writes, exactly as the preset runs it,
//! kept under `$STACK_STATE_DIR/templates/postgres/<key>/` (`data/`, the cluster with its
//! `.mise-daemon-version`, and `template.json`). The key ([`Recipe`]) is: the `initdb` that runs
//! and the `postgres` beside it (resolved path and file identity; the `postgres` first on PATH
//! must be that file); the install those come from, as [`install_snapshot`] records it; the
//! arguments; `LC_*`, `LANG` and `TZ`; `/etc/localtime`; the operating system release and, on
//! Linux, the system C library and locale archive by file identity (see [`system`]); the user;
//! the platform and [`FORMAT`]. A different key is another template; nothing is updated in place.
//!
//! - **Built once.** The first data directory that needs a template takes `<key>.lock`, runs
//!   `initdb` into `.build-<key>-*`, flushes the marker it adds, records a digest of every entry
//!   of the cluster (path, kind, mode, size, inode, link count, modification and change time) and
//!   renames the whole build to `<key>` once the record is flushed (on macOS with one
//!   `F_FULLFSYNC`, so the template survives power loss; one that could not be fully flushed is
//!   bound to the boot it was built in). Others wait for the lock (up to [`BUILD_WAIT`]) and clone
//!   the result. A build that died is never used: only a complete one is renamed into place, and
//!   the next builder removes what a dead one left.
//! - **Only a template like any other.** No template is built for a release whose `pg_control`
//!   layout is not in [`LAYOUTS`]. A build is published only when the identity edit accepts it
//!   ([`check_editable`]: a verified layout, a matching checksum, WAL segments of that layout and
//!   cluster) and `initdb` chose the settings it chooses on an unconstrained machine
//!   ([`UNCONSTRAINED`]). A build the edit refuses may be damaged: it is discarded, and the data
//!   directory is initialised by `initdb`. One that probed a machine short of shared memory is
//!   used for that one data directory and not kept.
//! - **Never changed unseen.** Stack never starts a server on a template. After each clone the
//!   template's digest is taken again and must be the one recorded at build: any write, chmod,
//!   link, rename or new file since then changes some entry's change time, which no one can set
//!   back, so the clone is of what was built. On a filesystem with whole-second times, a template
//!   is used only once its newest entry is [`SETTLE`] old. A template that fails the check is
//!   never used: the data directory is initialised by `initdb` and the template retired (see
//!   [`retire`]).
//! - **Each clone its own cluster.** Postgres stamps a cluster with a random system identifier
//!   at `initdb` (in `pg_control` and the long header of each WAL segment), and replication,
//!   `pg_rewind`, WAL archives and backup tools tell clusters apart by it. A clone is given a
//!   new one, made as Postgres makes it (time and process id), written in both places, and a
//!   new mock authentication nonce, with `pg_control`'s checksum recomputed, at the offsets of
//!   its verified layout; see [`distinguish`]. Nothing else in an initialised cluster names it:
//!   the data directory's path comes from `-D`, `postmaster.pid` and `postmaster.opts` exist only
//!   once a server runs, and the cluster has no tablespaces or logs.
//! - **Anything else is `initdb`.** A key that cannot be made, a template that cannot be built or
//!   cloned (on macOS anything but `clonefile`), or a clone that cannot be told apart from its
//!   template: the data directory is initialised by `initdb` as before, and the reason goes to
//!   the service's log. A template build whose `initdb` fails is retried once at the data
//!   directory. A template that the identity edit refuses (checked on the template itself when
//!   a copy fails) or that changed since it was built is never used; the first start that can
//!   take its lock tries to remove it, and the log says what became of it ([`retire`]).
//!
//! `stack gc` removes templates whose install changed or is gone, and templates no clone was
//! made from in [`UNUSED`].

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Bumped whenever what a template holds, or how it is keyed or checked, changes.
pub const FORMAT: &str = "stack-pg-template-3";
/// The arguments the preset runs `initdb` with, besides `-D`.
pub const INITDB_ARGS: &[&str] = &["-U", "postgres", "--auth=trust"];
/// The environment `initdb` reads that shapes what it writes: the cluster's locale and
/// encoding, `lc_*` and the `timezone`/`log_timezone` settings.
const ENV: &[&str] = &["LC_ALL", "LC_COLLATE", "LC_CTYPE", "LC_MESSAGES", "LC_MONETARY", "LC_NUMERIC", "LC_TIME", "LANG", "TZ"];
/// How long a first start waits for another to finish building the template it needs before
/// running `initdb` itself.
pub const BUILD_WAIT: Duration = Duration::from_secs(60);
/// See the module documentation: an install or template entry must be at least this old.
pub const SETTLE: Duration = crate::provider::env_cache::SETTLE;
/// How long a start whose copy of a template failed waits for the template's lock to remove it.
pub const RETIRE_WAIT: Duration = Duration::from_secs(5);
/// `stack gc` removes a template no data directory was made from in this long.
pub const UNUSED: Duration = Duration::from_secs(14 * 24 * 3600);
/// What `initdb` writes in `postgresql.conf` when its probes are not constrained by the machine
/// (verified on Postgres 17 and 18): a template is kept only with these.
pub const UNCONSTRAINED: &[(&str, &str)] = &[("max_connections", "100"), ("shared_buffers", "128MB")];
const RECORD: &str = "template.json";
const USED: &str = "used";

/// Where Stack keeps Postgres templates.
pub fn root(state: &Path) -> PathBuf {
    state.join("templates").join("postgres")
}

/// The first executable `name` on `path`, as a command started by name would run it. `None`
/// when an empty or relative entry comes first (it would resolve against the working
/// directory, which identifies nothing).
pub fn find(path: &OsStr, name: &str) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() || dir.is_relative() {
            return None;
        }
        let candidate = dir.join(name);
        if fs::metadata(&candidate).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) {
            return Some(candidate);
        }
    }
    None
}

/// What one template is keyed by, and the install it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    /// Every input, one per line.
    pub text: String,
    /// A digest of `text`: the template's directory name.
    pub key: String,
    pub install: PathBuf,
    /// A digest of the install's inputs (see [`install_snapshot`]).
    pub install_snapshot: String,
    /// The latest change time among the recorded install entries.
    newest: Option<SystemTime>,
}

/// One entry's identity and times, without following a link.
fn describe(meta: &fs::Metadata) -> String {
    let kind = if meta.file_type().is_symlink() {
        'l'
    } else if meta.is_dir() {
        'd'
    } else if meta.is_file() {
        'f'
    } else {
        'o'
    };
    format!(
        "{kind}:{:o}:{}:{}:{}:{}:{}.{}:{}.{}",
        meta.mode(),
        meta.dev(),
        meta.ino(),
        meta.nlink(),
        meta.size(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec()
    )
}

fn changed_at(meta: &fs::Metadata) -> Option<SystemTime> {
    let secs = u64::try_from(meta.ctime()).ok()?;
    let nanos = u32::try_from(meta.ctime_nsec()).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::new(secs, nanos))
}

/// What a fingerprint has seen so far.
#[derive(Default)]
struct Fingerprint {
    text: String,
    newest: Option<SystemTime>,
}

impl Fingerprint {
    fn note(&mut self, meta: &fs::Metadata) {
        if let Some(at) = changed_at(meta) {
            self.newest = Some(self.newest.map_or(at, |n| n.max(at)));
        }
    }

    /// One path: its own identity and, for a symbolic link, the link's text and the identity of
    /// what it leads to (a link that dangles or loops makes no fingerprint).
    fn entry(&mut self, label: &str, path: &Path) -> Result<fs::Metadata, String> {
        let own = fs::symlink_metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        self.note(&own);
        let mut line = format!("{label}\0{}", describe(&own));
        let followed = if own.file_type().is_symlink() {
            let text = fs::read_link(path).map_err(|e| format!("cannot read the link {}: {e}", path.display()))?;
            let target = fs::metadata(path).map_err(|e| format!("the link {} leads nowhere usable: {e}", path.display()))?;
            self.note(&target);
            line.push_str(&format!("\0-> {}\0{}", text.display(), describe(&target)));
            target
        } else {
            own
        };
        line.push('\n');
        self.text.push_str(&line);
        Ok(followed)
    }

    /// A directory (followed if it is a link) and every entry directly inside it, each as
    /// [`Fingerprint::entry`] records it.
    fn listing(&mut self, label: &str, dir: &Path) -> Result<(), String> {
        let meta = self.entry(label, dir)?;
        if !meta.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
        let mut names: Vec<OsString> = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?.map(|e| e.map(|e| e.file_name())).collect::<Result<_, _>>().map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        names.sort();
        for name in names {
            self.entry(&format!("\t{label}/{}", name.to_string_lossy()), &dir.join(&name))?;
        }
        Ok(())
    }
}

/// The inputs of `initdb` in the install at `install`, by metadata, without walking it all: the
/// install directory, `bin`, `lib`, `lib/postgresql` when present, each share directory (one
/// holding `postgres.bki`: `share`, `share/postgresql` or both), its `extension`, `timezonesets`
/// and `tsearch_data`, each with every entry directly inside, and the `plpgsql` module `initdb`
/// loads. A symbolic link among them is recorded with its text and what it leads to. Returns a
/// digest and the latest change time among them, or why there is none (no share directory,
/// no `plpgsql` module, a link that dangles or loops, anything unreadable).
pub fn install_snapshot(install: &Path) -> Result<(String, Option<SystemTime>), String> {
    let mut print = Fingerprint::default();
    // Which one initdb reads is compiled into it: every one that could be is recorded.
    let shares: Vec<&str> = ["share", "share/postgresql"].into_iter().filter(|rel| install.join(rel).join("postgres.bki").is_file()).collect();
    if shares.is_empty() {
        return Err(format!("no postgres.bki in {0}/share or {0}/share/postgresql", install.display()));
    }
    print.listing(".", install)?;
    print.listing("bin", &install.join("bin"))?;
    print.listing("lib", &install.join("lib"))?;
    if install.join("lib/postgresql").exists() {
        print.listing("lib/postgresql", &install.join("lib/postgresql"))?;
    }
    for share_rel in shares {
        let share = install.join(share_rel);
        print.listing(share_rel, &share)?;
        print.listing(&format!("{share_rel}/extension"), &share.join("extension"))?;
        for sub in ["timezonesets", "tsearch_data"] {
            if share.join(sub).exists() {
                print.listing(&format!("{share_rel}/{sub}"), &share.join(sub))?;
            }
        }
    }
    let module = ["lib/postgresql", "lib"]
        .iter()
        .flat_map(|dir| ["plpgsql.so", "plpgsql.dylib"].map(|f| format!("{dir}/{f}")))
        .find(|rel| install.join(rel).exists())
        .ok_or_else(|| format!("no plpgsql module in {}/lib", install.display()))?;
    print.entry(&format!("module {module}"), &install.join(&module))?;
    Ok((crate::hash::sha256_hex(print.text.as_bytes()), print.newest))
}

fn uname() -> String {
    // SAFETY: uname fills the zeroed struct it is given.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    let field = |f: &[libc::c_char]| {
        #[allow(clippy::unnecessary_cast)]
        let bytes: Vec<u8> = f.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    // SAFETY: as above.
    if unsafe { libc::uname(&mut u) } == 0 {
        format!("{} {} {}", field(&u.sysname), field(&u.release), field(&u.version))
    } else {
        "unknown".into()
    }
}

/// Record in `print` every `libc.so.6`, `ld-musl-*` loader and `libc.musl-*` directly in each of
/// `bases` or in one of their `*-linux-*` multiarch directories (`x86_64-linux-gnu`), as
/// [`Fingerprint::entry`] does; how many. A directory that cannot be listed (other than one
/// that does not exist) is an error, as a link that dangles is.
#[cfg(any(target_os = "linux", test))]
fn c_libraries(print: &mut Fingerprint, bases: &[&Path]) -> Result<usize, String> {
    let is_libc = |name: &str| name == "libc.so.6" || name.starts_with("ld-musl-") || name.starts_with("libc.musl-");
    // A directory that is not there holds nothing; one that cannot be listed may hold the C
    // library, so it makes no key.
    let sorted = |dir: &Path| -> Result<Vec<String>, String> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("cannot list {}: {e}", dir.display())),
        };
        let mut names = entries.map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned())).collect::<Result<Vec<_>, _>>().map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        names.sort();
        Ok(names)
    };
    // Whether `path` (followed) is a directory: not there is not one; unreadable is no key.
    let is_dir = |path: &Path| match fs::metadata(path) {
        Ok(meta) => Ok(meta.is_dir()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut found = 0;
    for base in bases {
        for name in sorted(base)? {
            let path = base.join(&name);
            if is_libc(&name) {
                print.entry(&path.display().to_string(), &path)?;
                found += 1;
            } else if name.contains("-linux-") && is_dir(&path)? {
                for lib in sorted(&path)?.into_iter().filter(|n| is_libc(n)) {
                    let lib = path.join(lib);
                    print.entry(&lib.display().to_string(), &lib)?;
                    found += 1;
                }
            }
        }
    }
    Ok(found)
}

/// The operating system release, and on Linux the system C library and locale data, whose
/// collation versions and locales `initdb` records. The C library is identified from outside
/// Stack's own binary (which may be static musl): every `libc.so.6`, `ld-musl-*` loader and
/// `libc.musl-*` in the standard library directories and their `*-linux-*` multiarch
/// directories ([`c_libraries`]), by resolved path and file identity, and the locale archive.
/// None found on Linux is no key. A server linked against a C library elsewhere (its own
/// `RPATH`) is not covered.
fn system() -> Result<String, String> {
    let mut out = uname();
    #[cfg(target_os = "linux")]
    {
        let mut print = Fingerprint::default();
        let found = c_libraries(&mut print, &[Path::new("/lib"), Path::new("/lib64"), Path::new("/usr/lib"), Path::new("/usr/lib64")])?;
        if found == 0 {
            return Err("cannot find the system C library (libc.so.6 or ld-musl) in /lib or /usr/lib".into());
        }
        for locale in ["/usr/lib/locale", "/usr/lib/locale/locale-archive"] {
            if Path::new(locale).exists() {
                print.entry(locale, Path::new(locale))?;
            }
        }
        out.push_str(&format!(" libc {}", crate::hash::sha256_hex(print.text.as_bytes())));
    }
    #[cfg(not(target_os = "linux"))]
    {
        out.push_str(" libc system");
    }
    Ok(out)
}

/// The system time zone `initdb` falls back to without `TZ`: `/etc/localtime`, the link and
/// the file it names.
fn localtime() -> String {
    let link = fs::read_link("/etc/localtime").map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
    let target = fs::metadata("/etc/localtime").map(|m| describe(&m)).unwrap_or_else(|_| "absent".into());
    format!("{link}\0{target}")
}

impl Recipe {
    /// The recipe for running `initdb` (found on PATH) with `env`, or why there is none.
    pub fn new(initdb: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Self, String> {
        let real = initdb.canonicalize().map_err(|e| format!("cannot resolve {}: {e}", initdb.display()))?;
        let bin = real.parent().ok_or("initdb has no directory")?;
        let install = bin.parent().ok_or("initdb's directory has no parent")?.to_path_buf();
        // initdb runs the `postgres` beside it; the preset runs the first on PATH. One cluster
        // for both only when they are the same file.
        let server = bin.join("postgres");
        let on_path = env("PATH").and_then(|p| find(&p, "postgres")).and_then(|p| p.canonicalize().ok());
        if on_path.as_deref() != Some(server.as_path()) {
            return Err(format!("the postgres on PATH is not the one beside {}", real.display()));
        }
        let (snapshot, newest) = install_snapshot(&install)?;
        let meta = |p: &Path| fs::metadata(p).map(|m| describe(&m)).map_err(|e| format!("cannot read {}: {e}", p.display()));
        let mut text = format!(
            "format={FORMAT}\nplatform={}-{}\nsystem={}\nuid={}\ninitdb={}\0{}\nserver={}\0{}\ninstall={}\0{snapshot}\nargs={}\nlocaltime={}\n",
            std::env::consts::OS,
            std::env::consts::ARCH,
            system()?,
            // SAFETY: geteuid has no preconditions.
            unsafe { libc::geteuid() },
            real.display(),
            meta(&real)?,
            server.display(),
            meta(&server)?,
            install.display(),
            INITDB_ARGS.join(" "),
            localtime(),
        );
        for name in ENV {
            match env(name) {
                Some(v) => text.push_str(&format!("env {name}={}\n", v.to_string_lossy())),
                None => text.push_str(&format!("env {name} unset\n")),
            }
        }
        let key = crate::hash::sha256_hex(text.as_bytes())[..24].to_string();
        Ok(Self { text, key, install, install_snapshot: snapshot, newest })
    }

    /// Whether the install last changed long enough ago to key a template by.
    fn settled(&self) -> bool {
        #[cfg(test)]
        if tests::INSTALLS_SETTLED.with(|s| s.get()) {
            return true;
        }
        self.newest.is_none_or(|at| SystemTime::now().duration_since(at).is_ok_and(|age| age >= SETTLE))
    }
}

/// What a template records about itself, in `template.json`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub format: String,
    pub recipe: String,
    /// The cluster's digest when it was built (see [`tree_digest`]).
    pub digest: String,
    /// The filesystem keeps whole seconds only: see [`SETTLE`].
    pub coarse: bool,
    pub newest_ms: u64,
    pub install: PathBuf,
    pub install_snapshot: String,
    pub built_at: u64,
    /// Everything was flushed to stable storage before the template was published (on macOS one
    /// `F_FULLFSYNC`; on Linux `fsync` is that already).
    pub full_flush: bool,
    /// Without `full_flush`, the boot it was built in: a power loss since may have lost what
    /// its digest cannot see, so after a reboot it is built again.
    pub boot: Option<String>,
}

/// A digest of `dir` and every entry under it (relative path, kind, mode, size, inode, link
/// count, modification and change time), whether every change time is a whole second, and the
/// latest one. Nothing is followed.
pub fn tree_digest(dir: &Path) -> std::io::Result<(String, bool, SystemTime)> {
    let mut lines = Vec::new();
    let mut coarse = true;
    let mut newest = SystemTime::UNIX_EPOCH;
    let mut pending = vec![PathBuf::new()];
    while let Some(rel) = pending.pop() {
        let path = dir.join(&rel);
        let meta = fs::symlink_metadata(&path)?;
        coarse &= meta.ctime_nsec() == 0;
        newest = newest.max(changed_at(&meta).unwrap_or(SystemTime::UNIX_EPOCH));
        lines.push(format!("{}\0{}", rel.display(), describe(&meta)));
        if meta.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(rel.join(entry?.file_name()));
            }
        }
    }
    lines.sort();
    Ok((crate::hash::sha256_hex(lines.join("\n").as_bytes()), coarse, newest))
}

/// Why no template made the data directory.
#[derive(Debug, PartialEq)]
pub enum Failure {
    /// Run `initdb` for this data directory instead; the reason is for the log.
    Fallback(String),
    /// `initdb` failed while building the template (its output is in the log). The caller runs
    /// it once more at the data directory: the failure may belong to the template's location.
    Initdb(String),
}

fn fallback(why: impl Into<String>) -> Failure {
    Failure::Fallback(why.into())
}

/// Run `initdb` as the preset does, into `dir` (which must not exist).
pub fn run_initdb(initdb: &Path, dir: &Path) -> Result<(), String> {
    let status = std::process::Command::new(initdb)
        .arg("-D")
        .arg(dir)
        .args(INITDB_ARGS)
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("cannot run initdb: {e}"))?;
    if !status.success() {
        return Err(format!("initdb failed ({status})"));
    }
    Ok(())
}

/// Write the preset's marker into the cluster at `data`, flushed.
pub fn write_marker(data: &Path, marker: &str) -> std::io::Result<()> {
    let mut file = fs::File::create(data.join(crate::supervisor::VERSION_FILE))?;
    file.write_all(marker.as_bytes())?;
    flush(&file)
}

fn now_ms(at: SystemTime) -> u64 {
    at.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn unique(parent: &Path, prefix: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    parent.join(format!("{prefix}{}-{nanos}", std::process::id()))
}

/// Remove a template, a build or a template set aside.
fn remove_tree(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

/// Move `dir` out of the way in one step (so no one clones a half-removed template), then
/// remove it. Once this returns `Ok` nothing is at `dir`; what was moved aside and could not be
/// removed is left for the next builder or `stack gc`.
fn discard(root: &Path, key: &str, dir: &Path) -> std::io::Result<()> {
    let aside = unique(root, &format!(".stale-{key}-"));
    fs::rename(dir, &aside)?;
    remove_tree(&aside);
    Ok(())
}

fn identity(dir: &Path) -> Option<(u64, u64)> {
    #[cfg(test)]
    tests::between_reads(0);
    fs::symlink_metadata(dir).ok().map(|m| (m.dev(), m.ino()))
}

/// The record of the template at `dir` for `recipe` (see [`read_record`]), read while that
/// directory stayed the same one (its identity before and after the read agree), so it is not
/// the record of a template a rebuild replaced meanwhile. `None` when there is no such template
/// or it was replaced while being read.
fn generation(dir: &Path, recipe: &Recipe) -> Option<Record> {
    unmoved(
        || identity(dir),
        || {
            #[cfg(test)]
            tests::between_reads(1);
            let record = read_record(dir, recipe);
            #[cfg(test)]
            tests::between_reads(2);
            record
        },
    )
}

/// What `read` gives, if `look` gave the same thing before it and after it: `look` runs first,
/// then `read`, then `look` again, in that order and no other, so that whatever `read` saw
/// belongs to what both looks saw.
fn unmoved<I: PartialEq, R>(look: impl Fn() -> Option<I>, read: impl FnOnce() -> Option<R>) -> Option<R> {
    let before = look()?;
    let read = read()?;
    (look()? == before).then_some(read)
}

/// What became of a template a failed copy said must go.
#[derive(Debug, PartialEq)]
enum Retired {
    Removed,
    /// The template itself passes the identity edit's check: only the copy was at fault.
    Kept,
    /// Its lock could not be taken within the wait: nothing was checked or removed.
    LockHeld,
    /// The directory is gone, or holds another template (a different record).
    Replaced,
    /// The lock could not be taken, or the template could not be read, checked or moved aside.
    Failed(String),
}

impl Retired {
    /// For the log, after "the template in <dir> ...".
    fn says(&self) -> String {
        match self {
            Self::Removed => "it was removed".into(),
            Self::Kept => "the template itself passes the check, so it was kept and only the copy discarded".into(),
            Self::LockHeld => "its lock could not be taken within the wait, so nothing was checked or removed; a later start that copies it tries again".into(),
            Self::Replaced => "it had already been replaced or removed, so nothing was removed".into(),
            Self::Failed(e) => format!("it was left in place: {e}; a later start that copies it tries again"),
        }
    }
}

/// Try to remove the template at `dir`, under its lock (`held`, or taken waiting at most
/// `wait`), and only while it still holds the record `copied` was made by (a rebuilt template's
/// record differs: its digest names other inodes). With `check`, the template is first checked
/// as the identity edit checks a copy ([`check_editable`], read-only), and kept if it passes.
fn retire(root: &Path, key: &str, dir: &Path, copied: &Record, check: bool, held: Option<crate::state::FileLock>, wait: Duration) -> Retired {
    let _guard = match held {
        Some(held) => held,
        None => match lock(root, key, wait) {
            Ok(lock) => lock,
            Err(LockError::Held(_)) => return Retired::LockHeld,
            Err(LockError::Failed(e)) => return Retired::Failed(e),
        },
    };
    let bytes = match fs::read(dir.join(RECORD)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && fs::symlink_metadata(dir).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) => return Retired::Replaced,
        Err(e) => return Retired::Failed(format!("cannot read its record: {e}")),
    };
    match serde_json::from_slice::<Record>(&bytes) {
        Ok(record) if record == *copied => {}
        Ok(_) => return Retired::Replaced,
        Err(e) => return Retired::Failed(format!("cannot read its record: {e}")),
    }
    if check {
        match check_editable(&dir.join("data")) {
            Ok(()) => return Retired::Kept,
            Err(e) if refused(&e) => {}
            Err(e) => return Retired::Failed(format!("cannot check it: {e}")),
        }
    }
    match discard(root, key, dir) {
        Ok(()) => Retired::Removed,
        Err(e) => Retired::Failed(format!("cannot move it aside: {e}")),
    }
}

fn read_any_record(dir: &Path) -> Option<Record> {
    serde_json::from_slice(&fs::read(dir.join(RECORD)).ok()?).ok()
}

/// The template's record when it is for exactly `recipe`, and was fully flushed or built in
/// this boot.
fn read_record(dir: &Path, recipe: &Recipe) -> Option<Record> {
    let record = read_any_record(dir)?;
    let durable = record.full_flush || (record.boot.is_some() && record.boot == crate::procinfo::boot_id());
    (record.format == FORMAT && record.recipe == recipe.text && durable).then_some(record)
}

/// Why the lock was not taken.
enum LockError {
    /// Someone held it for the whole wait.
    Held(String),
    /// It could not be opened or locked.
    Failed(String),
}

impl From<LockError> for Failure {
    fn from(e: LockError) -> Self {
        match e {
            LockError::Held(why) | LockError::Failed(why) => fallback(why),
        }
    }
}

/// The lock serialising builds of one template; held until dropped.
fn lock(root: &Path, key: &str, wait: Duration) -> Result<crate::state::FileLock, LockError> {
    let path = root.join(format!("{key}.lock"));
    let until = Instant::now() + wait;
    let mut pause = Duration::from_millis(2);
    loop {
        match crate::state::FileLock::try_acquire(&path) {
            Ok(Some(lock)) => return Ok(lock),
            Ok(None) if Instant::now() >= until => return Err(LockError::Held(format!("another first start has held {} for {} ms", path.display(), wait.as_millis()))),
            Ok(None) => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(25));
            }
            Err(e) => return Err(LockError::Failed(format!("cannot lock {}: {}", path.display(), e.message))),
        }
    }
}

/// Why the cluster `initdb` wrote in `data`, which [`distinguish`] can edit, is still not kept as
/// a template, if it is not: settings `initdb` chose on a constrained machine.
fn unpublishable(data: &Path) -> Option<String> {
    let conf = match fs::read_to_string(data.join("postgresql.conf")) {
        Ok(conf) => conf,
        Err(e) => return Some(format!("cannot read its postgresql.conf: {e}")),
    };
    for (name, value) in UNCONSTRAINED {
        let set = conf.lines().find_map(|line| {
            let (key, rest) = line.split_once('=')?;
            (key.trim() == *name).then(|| rest.split('#').next().unwrap_or_default().trim().to_string())
        });
        if set.as_deref() != Some(*value) {
            return Some(format!("initdb chose {name} = {} (not {value}: the machine was short of resources)", set.unwrap_or_else(|| "nothing".into())));
        }
    }
    None
}

/// Flush everything written so far to stable storage, once, if the platform can: on macOS
/// `F_FULLFSYNC` (which empties the drive's cache, so what `initdb` and Stack `fsync`ed is
/// durable); on Linux `fsync` already is. False when it cannot.
fn full_flush(file: &fs::File) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: a valid open descriptor.
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) != -1 }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = file;
        true
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = file;
        false
    }
}

/// What a build gave.
enum Built {
    /// Published as the template.
    Template(Record),
    /// Not kept: moved to the data directory's target as its own cluster; why it was not kept.
    Private(String),
}

/// Build the template for `recipe` under its lock (the caller holds it): `initdb` into a
/// build directory, the flushed marker, the check that its copies can be given their own
/// identity ([`check_editable`]), then either the digest, the record, a full flush and one
/// rename into place, or (see [`unpublishable`]) the build moved to `target` for that data
/// directory alone. A build that fails the check is discarded, never moved to `target` and
/// never published: the caller runs `initdb` at the data directory.
fn build(root: &Path, recipe: &Recipe, initdb: &Path, marker: &str, target: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Built, Failure> {
    let key = &recipe.key;
    let dir = root.join(key);
    // Whatever a build that died, or a removal that stopped, left: no one else holds the lock.
    for entry in fs::read_dir(root).map_err(|e| fallback(format!("cannot read {}: {e}", root.display())))?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&format!(".build-{key}-")) || name.starts_with(&format!(".stale-{key}-")) {
            remove_tree(&entry.path());
        }
    }
    if fs::symlink_metadata(&dir).is_ok() {
        discard(root, key, &dir).map_err(|e| fallback(format!("cannot move the old template in {} aside: {e}", dir.display())))?;
    }
    let staging = unique(root, &format!(".build-{key}-"));
    fs::DirBuilder::new().mode(0o700).create(&staging).map_err(|e| fallback(format!("cannot create {}: {e}", staging.display())))?;
    let made = (|| {
        let data = staging.join("data");
        run_initdb(initdb, &data).map_err(Failure::Initdb)?;
        write_marker(&data, marker).map_err(|e| fallback(format!("cannot write the marker: {e}")))?;
        // A pg_control or WAL segment the identity edit refuses may be a damaged cluster: neither
        // a template nor this data directory.
        check_editable(&data).map_err(|e| fallback(format!("the template build cannot be given its copies' own identity, so it was discarded: {e}")))?;
        if let Some(why) = unpublishable(&data) {
            fs::rename(&data, target).map_err(|e| fallback(format!("{why}; and cannot move the build to the data directory: {e}")))?;
            return Ok(Built::Private(why));
        }
        let (digest, coarse, newest) = tree_digest(&data).map_err(|e| fallback(format!("cannot read the new template: {e}")))?;
        // The install must be the one keyed before the build: one changed meanwhile may have
        // given initdb files of either.
        if Recipe::new(initdb, env).ok().as_ref() != Some(recipe) {
            return Err(fallback("the Postgres install changed while its template was built"));
        }
        let mut record = Record {
            format: FORMAT.into(),
            recipe: recipe.text.clone(),
            digest,
            coarse,
            newest_ms: now_ms(newest),
            install: recipe.install.clone(),
            install_snapshot: recipe.install_snapshot.clone(),
            built_at: crate::state::now(),
            full_flush: true,
            boot: None,
        };
        let write = |record: &Record| -> std::io::Result<fs::File> {
            let mut file = fs::File::create(staging.join(RECORD))?;
            file.write_all(&serde_json::to_vec_pretty(record).expect("record serializes"))?;
            flush(&file)?;
            Ok(file)
        };
        let file = write(&record).map_err(|e| fallback(format!("cannot write the template record: {e}")))?;
        if !full_flush(&file) {
            record.full_flush = false;
            record.boot = crate::procinfo::boot_id();
            if record.boot.is_none() {
                fs::rename(&data, target).map_err(|e| fallback(format!("cannot move the build to the data directory: {e}")))?;
                return Ok(Built::Private("it could not be flushed to stable storage and this boot cannot be identified".into()));
            }
            write(&record).map_err(|e| fallback(format!("cannot write the template record: {e}")))?;
        }
        fs::rename(&staging, &dir).map_err(|e| fallback(format!("cannot move the template into place: {e}")))?;
        Ok(Built::Template(record))
    })();
    if !matches!(made, Ok(Built::Template(_))) {
        remove_tree(&staging);
    }
    made
}

/// Make `target` (which must not exist; its parent must) a cluster for `recipe` from its
/// template, building the template first when there is none. Returns what was done, for the
/// log.
pub fn initialise(root: &Path, recipe: &Recipe, initdb: &Path, marker: &str, target: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> Result<String, Failure> {
    initialise_waiting(root, recipe, initdb, marker, target, env, (BUILD_WAIT, RETIRE_WAIT))
}

/// [`initialise`], waiting at most `waits.0` for another start building the template and
/// `waits.1` for its lock to remove a template whose copy failed.
fn initialise_waiting(root: &Path, recipe: &Recipe, initdb: &Path, marker: &str, target: &Path, env: &dyn Fn(&str) -> Option<OsString>, waits: (Duration, Duration)) -> Result<String, Failure> {
    let (wait, retire_wait) = waits;
    let started = Instant::now();
    if !recipe.settled() {
        return Err(fallback(format!("the Postgres install in {} changed in the last {}s", recipe.install.display(), SETTLE.as_secs())));
    }
    let dir = root.join(&recipe.key);
    let mut built = false;
    let mut held = None;
    // Without the lock, a template being replaced as it is read is read again under the lock.
    let record = match generation(&dir, recipe) {
        Some(record) => record,
        None => {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(root).map_err(|e| fallback(format!("cannot create {}: {e}", root.display())))?;
            held = Some(lock(root, &recipe.key, wait)?);
            match generation(&dir, recipe) {
                // Built while this one waited.
                Some(record) => record,
                None => match build(root, recipe, initdb, marker, target, env)? {
                    Built::Template(record) => {
                        built = true;
                        record
                    }
                    Built::Private(why) => return Ok(format!("initialised by initdb ({} ms); not kept as a template: {why}", started.elapsed().as_millis())),
                },
            }
        }
    };
    // The builder holds the lock throughout and its build directory was named by no one else.
    let recent = SystemTime::UNIX_EPOCH + Duration::from_millis(record.newest_ms);
    if record.coarse && !built && SystemTime::now().duration_since(recent).map_or(true, |age| age < SETTLE) {
        return Err(fallback(format!("the template in {} is on a filesystem with whole-second times and was built in the last {}s", dir.display(), SETTLE.as_secs())));
    }
    let cloned = clone_tree(&dir.join("data"), target);
    // The template must still be exactly what was built: then the clone, made since, is too.
    // (A template replaced since its record was read fails this too: the digest names inodes.
    // `retire` then leaves the new one alone.)
    let unchanged = tree_digest(&dir.join("data")).is_ok_and(|(digest, ..)| digest == record.digest);
    if !unchanged {
        let _ = fs::remove_dir_all(target);
        let retired = retire(root, &recipe.key, &dir, &record, false, held.take(), retire_wait);
        return Err(fallback(format!("the template in {} changed since it was built; {}", dir.display(), retired.says())));
    }
    let how = match cloned {
        Ok(how) => how,
        Err(e) => {
            let _ = fs::remove_dir_all(target);
            return Err(fallback(format!("cannot clone the template in {}: {e}", dir.display())));
        }
    };
    let system_identifier = match distinguish(target) {
        Ok(id) => id,
        // The fault may be the copy's (damaged since the clone, a full disk) or the template's
        // (every copy of it would be refused): the template itself is checked, and goes only if
        // it is refused too.
        Err(e) => {
            let _ = fs::remove_dir_all(target);
            let retired = retire(root, &recipe.key, &dir, &record, true, held.take(), retire_wait);
            return Err(fallback(format!("cannot give the copy of the template in {} its own identity ({e}); {}", dir.display(), retired.says())));
        }
    };
    // When a data directory was last made from it, for `stack gc`. Best effort, without the
    // lock: a `gc` that decided before this refresh may still remove the template, which costs
    // the next start one build (this clone is complete and checked already).
    let _ = fs::File::create(dir.join(USED));
    Ok(format!(
        "initialised from the template in {} ({}{how}, {} ms), system identifier {system_identifier}",
        dir.display(),
        if built { "built now, " } else { "" },
        started.elapsed().as_millis()
    ))
}

/// Make `target` (which must not exist; its parent must) a new cluster with the preset's
/// `marker`, as the service's `env` finds Postgres: from a template under `templates` when one
/// can be used (see [`initialise`]), else by `initdb` at `target`, which also runs once more
/// when the template's own `initdb` failed (both errors are reported if it fails too). Returns
/// what to say in the service's log. A release without a verified [`Layout`] (by the major in
/// `marker`, `postgres:<major>`) makes no template: its build could never be published.
pub fn make(templates: Option<&Path>, marker: &str, target: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Vec<String>, String> {
    let initdb = env("PATH").and_then(|path| find(&path, "initdb"));
    let major = marker.rsplit_once(':').map_or(marker, |(_, m)| m);
    let verified = LAYOUTS.iter().any(|l| major.parse() == Ok(l.major));
    let attempt = match (templates, &initdb) {
        (None, _) => Err(Failure::Fallback(String::new())),
        (Some(_), _) if !verified => Err(fallback(format!("no template for Postgres {major}: Stack has not verified its pg_control layout"))),
        (Some(_), None) => Err(fallback("initdb is not found on PATH by an absolute directory")),
        (Some(root), Some(initdb)) => Recipe::new(initdb, env).map_err(Failure::Fallback).and_then(|recipe| initialise(root, &recipe, initdb, marker, target, env)),
    };
    let (notes, earlier) = match attempt {
        Ok(note) => return Ok(vec![note]),
        Err(Failure::Fallback(why)) if why.is_empty() => (Vec::new(), None),
        Err(Failure::Fallback(why)) => (vec![format!("running initdb for this data directory: {why}")], None),
        Err(Failure::Initdb(e)) => (vec![format!("the template's initdb failed ({e}); running initdb for this data directory")], Some(e)),
    };
    // Whatever an attempt left in the caller's staging directory.
    let _ = fs::remove_dir_all(target);
    let program = initdb.unwrap_or_else(|| PathBuf::from("initdb"));
    let made = run_initdb(&program, target).and_then(|()| write_marker(target, marker).map_err(|e| format!("cannot write {}: {e}", target.display())));
    match (made, earlier) {
        (Ok(()), _) => Ok(notes),
        (Err(e), Some(first)) => Err(format!("the template's initdb failed ({first}), and the data directory could not be made either ({e})")),
        // Why the template was not used, and what became of it, is still said.
        (Err(e), None) if !notes.is_empty() => Err(format!("{}; and the data directory could not be made: {e}", notes.join("; "))),
        (Err(e), None) => Err(e),
    }
}

// ---- copying -------------------------------------------------------------------------------

/// Copy the directory `src` to `dst` (which must not exist), modes and times kept where the
/// platform clones. On macOS one `clonefile` or nothing (an error: a plain copy saves little
/// over `initdb`, and would need every file flushed). Elsewhere directory by directory, each
/// file reflinked (`FICLONE` on Linux, which Btrfs and XFS support) or copied, then the
/// filesystem flushed (`syncfs`). Only directories and regular files are copied.
pub fn clone_tree(src: &Path, dst: &Path) -> std::io::Result<&'static str> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        const CLONE_NOFOLLOW: u32 = 0x0001;
        let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e));
        let (s, d) = (c(src)?, c(dst)?);
        // SAFETY: both are valid NUL-terminated paths; clonefile only reads them.
        if unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), CLONE_NOFOLLOW) } == 0 {
            return Ok("clonefile");
        }
        Err(std::io::Error::last_os_error())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut reflinked = true;
        copy_dir(src, dst, &mut reflinked)?;
        #[cfg(target_os = "linux")]
        {
            // A plain copy wrote every byte: on disk before the cluster is used, as initdb's are.
            let dir = fs::File::open(dst)?;
            use std::os::fd::AsRawFd;
            // SAFETY: a valid open descriptor.
            if unsafe { libc::syncfs(dir.as_raw_fd()) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(if reflinked { "reflink" } else { "copy" })
    }
}

#[cfg(not(target_os = "macos"))]
fn copy_dir(src: &Path, dst: &Path, reflinked: &mut bool) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    fs::DirBuilder::new().mode(0o700).create(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        let m = fs::symlink_metadata(&from)?;
        if m.is_dir() {
            copy_dir(&from, &to, reflinked)?;
        } else if m.is_file() {
            copy_file(&from, &to, &m, reflinked)?;
        } else {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{} is not a regular file or directory", from.display())));
        }
    }
    fs::set_permissions(dst, fs::Permissions::from_mode(meta.mode() & 0o7777))
}

#[cfg(not(target_os = "macos"))]
fn copy_file(from: &Path, to: &Path, meta: &fs::Metadata, reflinked: &mut bool) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let source = fs::File::open(from)?;
        let dest = fs::OpenOptions::new().write(true).create_new(true).mode(meta.mode() & 0o7777).open(to)?;
        // SAFETY: two valid open descriptors; FICLONE shares the source's extents or fails.
        if unsafe { libc::ioctl(dest.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) } == 0 {
            return Ok(());
        }
        drop(dest);
        fs::remove_file(to)?;
    }
    *reflinked = false;
    fs::copy(from, to)?;
    fs::set_permissions(to, fs::Permissions::from_mode(meta.mode() & 0o7777))
}

// ---- telling clusters apart ----------------------------------------------------------------

/// The size of `pg_control`'s contents that Postgres reads and checksums (`PG_CONTROL_MAX_SAFE_SIZE`).
const CONTROL_SAFE: usize = 512;
/// Where `pg_control` keeps `pg_control_version`, after the 8-byte system identifier.
const CONTROL_VERSION_AT: usize = 8;
/// `XLP_LONG_HEADER`: the first page of a WAL segment, whose header names the system.
const XLP_LONG_HEADER: u16 = 0x0002;
/// Where a long WAL page header keeps the system identifier (after the 24-byte short header).
const XLP_SYSID: usize = 24;
/// `MOCK_AUTH_NONCE_LEN`.
pub const NONCE_LEN: usize = 32;

/// A `pg_control` and WAL layout [`distinguish`] may edit.
#[derive(Debug, PartialEq)]
pub struct Layout {
    /// The Postgres major release.
    pub major: u32,
    /// `PG_CONTROL_VERSION`, at offset 8 of `pg_control`.
    pub control_version: u32,
    /// `XLOG_PAGE_MAGIC`, the first two bytes of every WAL page.
    pub wal_magic: u16,
    /// `offsetof(ControlFileData, mock_authentication_nonce)`.
    pub nonce: usize,
    /// `offsetof(ControlFileData, crc)`.
    pub crc: usize,
}

/// The layouts verified with `offsetof` against each release's installed server headers
/// (`catalog/pg_control.h`, `access/xlog_internal.h`): Postgres 17.6, 17.11 and 17.12 (version
/// 1700), 18.6 and 18.7 (1800, where `default_char_signedness` moved the nonce one byte and three
/// bytes of padding precede the checksum). The system identifier is at 0 in `pg_control` and at
/// 24 in a long WAL page header in both. Anything else is not edited: `initdb` runs.
pub const LAYOUTS: &[Layout] = &[
    Layout { major: 17, control_version: 1700, wal_magic: 0xD116, nonce: 256, crc: 288 },
    Layout { major: 18, control_version: 1800, wal_magic: 0xD118, nonce: 257, crc: 292 },
];

/// CRC-32C (Castagnoli), as Postgres checksums `pg_control`.
pub fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82F6_3B78 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// The verified layout of the `pg_control` contents `control` (its first [`CONTROL_SAFE`]
/// bytes), checked: a known `pg_control_version`, and the checksum where that layout keeps it
/// matching. Postgres stores these in native byte order; only little-endian is verified.
pub fn layout(control: &[u8]) -> Result<&'static Layout, String> {
    if cfg!(target_endian = "big") {
        return Err("a big-endian pg_control layout is not verified".into());
    }
    let version = u32::from_le_bytes(control[CONTROL_VERSION_AT..CONTROL_VERSION_AT + 4].try_into().expect("4 bytes"));
    let layout = LAYOUTS.iter().find(|l| l.control_version == version).ok_or_else(|| format!("pg_control version {version} is not one whose layout Stack verified"))?;
    if crc32c(&control[..layout.crc]).to_le_bytes() != control[layout.crc..layout.crc + 4] {
        return Err(format!("pg_control version {version}: the checksum at {} does not match", layout.crc));
    }
    Ok(layout)
}

/// A system identifier as `initdb` makes one: seconds and microseconds now, and the low bits of
/// the process id.
fn new_system_identifier() -> u64 {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    (now.as_secs() << 32) | (u64::from(now.subsec_micros()) << 12) | (u64::from(std::process::id()) & 0xFFF)
}

/// `fsync`, as `initdb` flushes what it writes (on macOS not `F_FULLFSYNC`, which
/// `File::sync_all` asks for and which flushes the whole drive's cache).
fn flush(file: &fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: a valid open descriptor.
    if unsafe { libc::fsync(file.as_raw_fd()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Give the cluster in `data` (a copy of a template, never started) its own system identifier
/// and mock authentication nonce, as `initdb` would have (see [`distinguish_with`]).
pub fn distinguish(data: &Path) -> std::io::Result<u64> {
    let mut nonce = [0u8; NONCE_LEN];
    fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut nonce)).map_err(|e| std::io::Error::other(format!("cannot read /dev/urandom: {e}")))?;
    #[cfg(test)]
    tests::before_edit(data);
    distinguish_with(data, new_system_identifier(), nonce)
}

/// Whether `e`, from [`check_editable`], is the check refusing the cluster's contents rather
/// than trouble reading them.
fn refused(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::InvalidData
}

/// The files of the cluster in `data` that [`distinguish_with`] edits, opened and checked.
struct Editable {
    layout: &'static Layout,
    control_file: fs::File,
    control: Vec<u8>,
    /// Every WAL segment, each with the layout's page magic, a long header and the cluster's
    /// identifier.
    headers: Vec<fs::File>,
}

/// Whether [`distinguish_with`] can edit the cluster in `data`: its `pg_control` has a verified
/// [`layout`] with a matching checksum, and every WAL segment carries that layout's page magic
/// and names the cluster. Nothing is written.
fn check_editable(data: &Path) -> std::io::Result<()> {
    editable(data, false).map(drop)
}

/// The cluster's contents refusing the edit are `InvalidData` (see [`refused`]): a layout or
/// WAL header it does not accept, and a file or directory missing or too short.
fn editable(data: &Path, write: bool) -> std::io::Result<Editable> {
    let open = |path: &Path| fs::OpenOptions::new().read(true).write(write).open(path);
    let invalid = |what: String| std::io::Error::new(std::io::ErrorKind::InvalidData, what);
    let content = |path: &Path| {
        let path = path.to_path_buf();
        move |e: std::io::Error| match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::UnexpectedEof => std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{}: {e}", path.display())),
            _ => e,
        }
    };
    let control_path = data.join("global").join("pg_control");
    let mut control_file = open(&control_path).map_err(content(&control_path))?;
    let mut control = vec![0u8; CONTROL_SAFE];
    control_file.read_exact(&mut control).map_err(content(&control_path))?;
    let layout = layout(&control).map_err(|why| invalid(format!("{}: {why}", control_path.display())))?;
    let old: [u8; 8] = control[..8].try_into().expect("8 bytes");
    let mut segments = Vec::new();
    for entry in fs::read_dir(data.join("pg_wal")).map_err(content(&data.join("pg_wal")))? {
        let entry = entry?;
        let name = entry.file_name();
        let segment = name.len() == 24 && name.to_string_lossy().bytes().all(|b| b.is_ascii_hexdigit());
        if segment && entry.file_type()?.is_file() {
            segments.push(entry.path());
        }
    }
    if segments.is_empty() {
        return Err(invalid("no WAL segment".into()));
    }
    let mut headers = Vec::new();
    for path in &segments {
        let mut file = open(path).map_err(content(path))?;
        let mut header = [0u8; XLP_SYSID + 8];
        file.read_exact(&mut header).map_err(content(path))?;
        let magic = u16::from_le_bytes([header[0], header[1]]);
        let info = u16::from_le_bytes([header[2], header[3]]);
        if magic != layout.wal_magic || info & XLP_LONG_HEADER == 0 || header[XLP_SYSID..] != old {
            return Err(invalid(format!("{}: not a WAL segment of this cluster", path.display())));
        }
        headers.push(file);
    }
    Ok(Editable { layout, control_file, control, headers })
}

/// Write `id` and `nonce` into the cluster in `data`: in `pg_control` at the offsets of its
/// verified [`layout`] (its checksum recomputed; nothing else changes) and in the long header of
/// every WAL segment, each of which must carry the layout's page magic and name the old
/// identifier (see [`check_editable`]). Each changed file is flushed before this returns.
/// Anything unexpected is an error before anything is written; the caller removes the copy on
/// any error.
pub fn distinguish_with(data: &Path, id: u64, nonce: [u8; NONCE_LEN]) -> std::io::Result<u64> {
    let Editable { layout, mut control_file, mut control, headers } = editable(data, true)?;
    #[cfg(test)]
    if tests::EDIT_WRITE_FAILS.with(|f| f.get()) {
        return Err(std::io::Error::new(std::io::ErrorKind::StorageFull, "No space left on device (injected)"));
    }
    control[..8].copy_from_slice(&id.to_le_bytes());
    control[layout.nonce..layout.nonce + NONCE_LEN].copy_from_slice(&nonce);
    let crc = crc32c(&control[..layout.crc]);
    control[layout.crc..layout.crc + 4].copy_from_slice(&crc.to_le_bytes());
    for mut file in headers {
        file.seek(SeekFrom::Start(XLP_SYSID as u64))?;
        file.write_all(&id.to_le_bytes())?;
        flush(&file)?;
    }
    control_file.seek(SeekFrom::Start(0))?;
    control_file.write_all(&control)?;
    flush(&control_file)?;
    Ok(id)
}

// ---- collection ----------------------------------------------------------------------------

/// A template `stack gc` removes, or would.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Unused {
    pub dir: PathBuf,
    pub reason: String,
    pub bytes: u64,
}

fn size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            total += meta.blocks().saturating_mul(512);
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    total
}

/// Why nothing can use the template in `dir` any more, if so: an unreadable record or another
/// format, an install that changed or is gone, one not fully flushed from an earlier boot, or no
/// clone made from it in [`UNUSED`].
fn unusable(dir: &Path) -> Option<String> {
    let Some(record) = read_any_record(dir) else {
        return Some("its record is missing or unreadable".into());
    };
    if record.format != FORMAT {
        return Some(format!("it is of another format ({})", record.format));
    }
    match install_snapshot(&record.install) {
        Ok((snapshot, _)) if snapshot == record.install_snapshot => {}
        Ok(_) => return Some(format!("its Postgres install in {} changed", record.install.display())),
        Err(why) => return Some(format!("its Postgres install in {} is gone or unreadable ({why})", record.install.display())),
    }
    if !record.full_flush && record.boot != crate::procinfo::boot_id() {
        return Some("it was not fully flushed and the machine has restarted since".into());
    }
    let last = fs::metadata(dir.join(USED)).or_else(|_| fs::metadata(dir.join(RECORD))).and_then(|m| m.modified()).ok()?;
    let idle = SystemTime::now().duration_since(last).unwrap_or_default();
    (idle >= UNUSED).then(|| format!("no data directory was made from it in {} days", idle.as_secs() / 86_400))
}

/// The templates under `state` nothing can use any more, and builds or removals a process left
/// behind; removed unless `dry_run`. Each is judged and removed under its recipe's lock, taken
/// without waiting (a dry run takes it too, and lets it go): one in use by a build is left for
/// the next pass, and one that changed between the listing and the lock (rebuilt, or the
/// entry gone) is judged as it is now.
pub fn collect(state: &Path, dry_run: bool) -> Vec<Unused> {
    let root = root(state);
    let Ok(entries) = fs::read_dir(&root) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let Ok(listed) = fs::symlink_metadata(&path) else { continue };
        if name.ends_with(".lock") || !listed.is_dir() {
            continue;
        }
        let leftover = name.starts_with(".build-") || name.starts_with(".stale-");
        let key = if leftover { name[7..].split('-').next().unwrap_or_default().to_string() } else { name.clone() };
        let Ok(Some(_lock)) = crate::state::FileLock::try_acquire(&root.join(format!("{key}.lock"))) else { continue };
        if fs::symlink_metadata(&path).map(|m| (m.dev(), m.ino())).ok() != Some((listed.dev(), listed.ino())) {
            continue;
        }
        let reason = if leftover {
            "a build or removal that did not finish".to_string()
        } else {
            match unusable(&path) {
                Some(reason) => reason,
                None => continue,
            }
        };
        let bytes = size(&path);
        if !dry_run {
            if leftover {
                remove_tree(&path);
            } else if discard(&root, &key, &path).is_err() {
                continue;
            }
        }
        out.push(Unused { dir: path, reason, bytes });
    }
    out
}

#[cfg(test)]
mod tests;

#![cfg(target_os = "linux")]
//! Real-kernel regressions for what Stack's mount and process checks exist for on Linux, which
//! the library tests show only against stand-ins for the kernel:
//!
//! - a bind mount of a directory of the same filesystem (one device, two mounts): a checkout
//!   under it deleted is gone; the bind mount taken down, leaving the empty directory it was on,
//!   is not taken for a deletion; mounted again, the checkout is judged by the new mount's id;
//! - a single file, or a directory, bind-mounted inside a data directory: its removal is refused
//!   before anything is removed, the mounted file untouched; once unmounted, it is removed;
//! - a fresh PID namespace with its own `/proc`: a lease owner named from outside, still running
//!   but invisible here, is unknown, never exited; one named here is judged by its start time.
//!
//! Nothing is mounted on the host: the checks run in this test binary re-run under `unshare` in a
//! user, mount (private propagation) and PID namespace of their own, as the root of that user
//! namespace only. Where unprivileged namespaces are unavailable the test says SKIPPED and passes,
//! unless `STACK_REQUIRE_LINUX_NAMESPACES` is set, when that fails it.

use stack::checkout::{self, Presence, Recorded};
use stack::datadirs::{self, Entry, Found};
use stack::lease::{self, OwnerIdentity, OwnerState};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Set in the re-run under `unshare`, naming the directory to work in.
const INSIDE: &str = "STACK_NAMESPACE_DIR";
const OWNER: &str = "STACK_NAMESPACE_OWNER";
const OWNER_IDENTITY: &str = "STACK_NAMESPACE_OWNER_IDENTITY";
/// What the checks print once all have passed, which the outer test requires.
const PASSED: &str = "namespace checks passed:";
const BOUND: Duration = Duration::from_secs(120);

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

/// `command`'s exit status and output, killed once it has run for [`BOUND`].
fn bounded(mut command: Command) -> (Option<i32>, String) {
    let dir = tempfile::tempdir().unwrap();
    let (out, err) = (dir.path().join("out"), dir.path().join("err"));
    let mut child = command
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&out).unwrap())
        .stderr(std::fs::File::create(&err).unwrap())
        .spawn()
        .unwrap();
    let until = Instant::now() + BOUND;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if Instant::now() > until {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let text = format!("{}{}", std::fs::read_to_string(&out).unwrap_or_default(), std::fs::read_to_string(&err).unwrap_or_default());
    (status, text)
}

#[test]
fn real_bind_mounts_and_pid_namespaces_are_judged_as_the_kernel_reports_them() {
    let required = std::env::var_os("STACK_REQUIRE_LINUX_NAMESPACES").is_some();
    let skip = |why: String| {
        assert!(!required, "STACK_REQUIRE_LINUX_NAMESPACES is set, but {why}");
        eprintln!("SKIPPED (the namespace checks did not run): {why}");
    };
    let Some(unshare) = on_path("unshare") else { return skip("there is no unshare on PATH".into()) };
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();

    // Whether this machine lets an unprivileged process mount in namespaces of its own (Ubuntu
    // 24.04 restricts it with AppArmor's `apparmor_restrict_unprivileged_userns`).
    let mut probe = Command::new(&unshare);
    probe.args(["--user", "--map-root-user", "--mount", "--propagation", "private", "--pid", "--fork", "--mount-proc", "sh", "-c"]);
    probe.arg(r#"mkdir "$1/probe-a" "$1/probe-b" && mount --bind "$1/probe-a" "$1/probe-b" && umount "$1/probe-b""#).arg("sh").arg(&base);
    let (status, output) = bounded(probe);
    if status != Some(0) {
        let restricted = std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").unwrap_or_default();
        return skip(format!(
            "an unprivileged user, mount and PID namespace cannot bind-mount here (exit {status:?}, apparmor_restrict_unprivileged_userns={}): {}",
            restricted.trim(),
            output.trim()
        ));
    }

    let owner = std::process::id();
    let identity = OwnerIdentity::observe(owner);
    assert!(identity.pid_ns.is_some() && identity.started.is_some(), "this process cannot be observed: {identity:?}");
    let mut inside = Command::new(&unshare);
    inside
        .args(["--user", "--map-root-user", "--mount", "--propagation", "private", "--pid", "--fork", "--mount-proc"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "inside_namespaces", "--ignored", "--nocapture", "--test-threads=1"])
        .env(INSIDE, &base)
        .env(OWNER, owner.to_string())
        .env(OWNER_IDENTITY, serde_json::to_string(&identity).unwrap());
    let (status, output) = bounded(inside);
    eprintln!("{output}");
    assert_eq!(status, Some(0), "the checks inside the namespaces failed (exit {status:?}; none if stopped after {BOUND:?})");
    assert!(output.contains("test result: ok. 1 passed") && output.contains(PASSED), "the checks did not report passing");
}

/// Mount `source` on `target` (a bind mount), in this process's own mount namespace.
fn bind(source: &Path, target: &Path) {
    let (s, t) = (CString::new(source.as_os_str().as_bytes()).unwrap(), CString::new(target.as_os_str().as_bytes()).unwrap());
    // SAFETY: valid NUL-terminated paths; a bind mount takes no filesystem type or data.
    let rc = unsafe { libc::mount(s.as_ptr(), t.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null()) };
    assert_eq!(rc, 0, "bind {} on {}: {}", source.display(), target.display(), std::io::Error::last_os_error());
}

fn unmount(target: &Path) {
    let t = CString::new(target.as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated path.
    let rc = unsafe { libc::umount2(t.as_ptr(), 0) };
    assert_eq!(rc, 0, "unmount {}: {}", target.display(), std::io::Error::last_os_error());
}

fn unknown(presence: Presence) -> String {
    match presence {
        Presence::Unknown(why) => why,
        other => panic!("expected unknown, got {other:?}"),
    }
}

/// A directory bind-mounted from the same filesystem holds the checkout.
fn same_device_directory_bind_mount(base: &Path) {
    let (srv, repos) = (base.join("srv"), base.join("repos"));
    std::fs::create_dir_all(srv.join("wt")).unwrap();
    std::fs::create_dir(&repos).unwrap();
    bind(&srv, &repos);
    let wt = repos.join("wt");
    // The case a device cannot tell: one filesystem, two mounts.
    assert_eq!(checkout::device(&repos), checkout::device(base), "the bind mount is of the same filesystem");
    let (here, under) = (stack::mounts::of_path(base).unwrap().unwrap(), stack::mounts::of_path(&repos).unwrap().unwrap());
    assert_ne!(here.id, under.id, "the kernel names the bind mount apart from the directory it is on");

    let recorded = Recorded::of(&wt);
    let location = recorded.location.clone().expect("a location on Linux");
    assert_eq!(location.mount_root, repos, "the checkout's mount is rooted at the bind mount: {location:?}");
    let mount = location.mount.clone().expect("the mount's id");
    assert_eq!(mount.id, under.id);

    // Deleted while its mount is there: gone.
    std::fs::remove_dir(srv.join("wt")).unwrap();
    assert_eq!(checkout::presence(&wt, &recorded), Presence::Absent);
    std::fs::create_dir(srv.join("wt")).unwrap();
    let recorded = Recorded::of(&wt);

    // The bind mount taken down: an empty directory of the same filesystem is where the
    // checkout's mount was, which a device alone took for a deletion.
    unmount(&repos);
    assert!(std::fs::read_dir(&repos).unwrap().next().is_none());
    assert_eq!(checkout::device(&repos), recorded.device, "the directory left is on the checkout's device");
    let why = unknown(checkout::presence(&wt, &recorded));
    assert!(why.contains("may not be there"), "{why}");
    assert!(!checkout::gone(&wt, &recorded));

    // Mounted again from the same directory and the checkout deleted from it: another mount,
    // which only the kernel's never-reused id tells apart (Linux 6.8).
    bind(&srv, &repos);
    std::fs::remove_dir(srv.join("wt")).unwrap();
    if recorded.location.as_ref().and_then(|l| l.mount.as_ref()).is_some_and(|m| m.unique.is_some()) {
        let why = unknown(checkout::presence(&wt, &recorded));
        assert!(why.contains("another mount id"), "{why}");
    } else {
        println!("no unique mount ids on this kernel: the remount case was not checked");
    }
    unmount(&repos);
}

/// A data directory as `up` records it, with a marker beside it.
fn data_entry(base: &Path, name: &str) -> (PathBuf, Entry) {
    let parent = base.join(name);
    let data = parent.join("data");
    std::fs::create_dir_all(data.join("sub")).unwrap();
    std::fs::write(data.join("a"), "123").unwrap();
    std::fs::write(data.join("sub/more"), "45").unwrap();
    std::fs::write(parent.join("data.stack-checkout"), "token").unwrap();
    let (location, identity) = datadirs::locate(&data);
    assert!(location.is_some() && identity.is_some());
    let entry = Entry {
        project: base.join("project"),
        service: "postgres".into(),
        dir: data.clone(),
        checkout: "token".into(),
        supervisor: None,
        location,
        identity,
        project_at: Recorded::default(),
    };
    (parent, entry)
}

fn held(entry: &Entry) -> datadirs::Held {
    match datadirs::hold(entry) {
        Found::Held(held) => held,
        Found::Gone => panic!("the data directory is gone"),
        Found::Other(why) | Found::Unbound(why) => panic!("{why}"),
    }
}

/// A single file, then a directory, bind-mounted inside a data directory.
fn bind_mounts_inside_data(base: &Path) {
    let outside = base.join("outside");
    std::fs::write(&outside, "SECRET").unwrap();
    let (parent, entry) = data_entry(base, "file-mounted");
    let more = parent.join("data/sub/more");
    bind(&outside, &more);
    assert_eq!(checkout::device(&parent), checkout::device(base));
    let e = held(&entry).remove(".stack-checkout").unwrap_err().to_string();
    assert!(e.contains("more inside it is a mount point") && e.contains("of the same filesystem") && e.contains("nothing was removed"), "{e}");
    assert_eq!(std::fs::read(parent.join("data/a")).unwrap(), b"123", "nothing was removed");
    assert_eq!(std::fs::read(&more).unwrap(), b"SECRET", "the mounted file is untouched");
    unmount(&more);
    assert_eq!(std::fs::read(&outside).unwrap(), b"SECRET");

    let elsewhere = base.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("kept"), "6789").unwrap();
    let sub = parent.join("data/sub");
    bind(&elsewhere, &sub);
    let e = held(&entry).remove(".stack-checkout").unwrap_err().to_string();
    assert!(e.contains("sub inside it is a mount point") && e.contains("of the same filesystem") && e.contains("nothing was removed"), "{e}");
    assert_eq!(std::fs::read(elsewhere.join("kept")).unwrap(), b"6789");
    unmount(&sub);

    // Nothing mounted inside: removed, marker and all, and nothing outside it.
    let removed = held(&entry).remove(".stack-checkout").unwrap();
    assert_eq!(removed, 5, "the bytes of data/a and data/sub/more");
    assert!(!parent.join("data").exists() && !parent.join("data.stack-checkout").exists());
    assert_eq!(std::fs::read(&outside).unwrap(), b"SECRET");
    assert_eq!(std::fs::read(elsewhere.join("kept")).unwrap(), b"6789");
}

/// This PID namespace's `/proc` is its own: the owner outside is not visible, and is unknown.
fn pid_namespace_visibility() {
    let owner: u32 = std::env::var(OWNER).unwrap().parse().unwrap();
    let identity: OwnerIdentity = serde_json::from_str(&std::env::var(OWNER_IDENTITY).unwrap()).unwrap();
    let here = stack::procinfo::pid_namespace().expect("this namespace's /proc names it");
    assert_ne!(Some(here), identity.pid_ns, "the checks run in a PID namespace of their own");
    assert!(stack::procinfo::process(std::process::id()).is_some(), "this /proc shows this process");
    println!("owner {owner} from outside, as this namespace's kernel sees it: {:?}", stack::procinfo::liveness(owner));
    match lease::owner_state(owner, Some(&identity)) {
        OwnerState::Unknown(why) => assert!(why.contains("another PID namespace"), "{why}"),
        other => panic!("an owner named from outside, still running, was judged {other:?}"),
    }

    // An owner named here is judged here: alive while it runs, exited once it has.
    let mut child = Command::new("sleep").arg("60").spawn().unwrap();
    let named = OwnerIdentity::observe(child.id());
    assert_eq!(named.pid_ns, Some(here));
    assert_eq!(lease::owner_state(child.id(), Some(&named)), OwnerState::Alive);
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(matches!(lease::owner_state(child.id(), Some(&named)), OwnerState::Exited(_)));
}

/// The checks themselves: run only by the test above, inside its namespaces.
#[test]
#[ignore = "run inside namespaces by real_bind_mounts_and_pid_namespaces_are_judged_as_the_kernel_reports_them"]
fn inside_namespaces() {
    let base = PathBuf::from(std::env::var_os(INSIDE).expect("run by real_bind_mounts_and_pid_namespaces_are_judged_as_the_kernel_reports_them"));
    // SAFETY: getuid cannot fail.
    assert_eq!(unsafe { libc::getuid() }, 0, "root of this user namespace only");
    same_device_directory_bind_mount(&base);
    bind_mounts_inside_data(&base);
    pid_namespace_visibility();
    println!("{PASSED} same-device directory bind mount, file and directory bind mounts inside data, PID namespace visibility");
}

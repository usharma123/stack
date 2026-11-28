//! Stopping processes Stack is not the parent of: a service process no record accounts for
//! (a stray), and what a supervisor that died left running. Ownership is established before
//! anything is signalled (the checkout's token in the process's environment, its start time,
//! the group its recorded leader still leads); this module only sends the signals, each bound
//! to the process observed:
//!
//! - On Linux every signal goes through a pidfd opened before the process's identity is checked
//!   again, so it reaches that process or none (`pidfd_open`, `pidfd_send_signal`). A group is
//!   never signalled by its number: its members are listed and signalled one by one, while its
//!   recorded leader still leads it. Without pidfd support (a kernel older than 5.3) nothing is
//!   signalled and every process is reported as not stopped.
//! - On macOS, which has no such handle, a process is signalled by pid right after its pid and
//!   start time were observed again, and a group by its number right after its leader was. The
//!   check and the signal are separate system calls: a process that exits in between and whose
//!   pid is handed to another process at once would receive the signal. That gap is documented
//!   in docs/user/guarantees.md; closing it needs a process that stays the parent.

use crate::procinfo::Process;
use serde_json::{json, Value};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// A process of this checkout to stop with its group: `chain` is the process and the ancestors
/// that also belong to the checkout, first one first, as observed; `group` is the topmost one's
/// process group when it leads it, and `members` every other process of that group known to be
/// the checkout's. `port`, when given, must be closed too before the stray counts as stopped.
/// `signal` is the first signal sent (the service's stop signal); `SIGKILL` follows.
#[derive(Debug, Clone)]
pub struct Stray {
    pub service: String,
    pub port: Option<u16>,
    pub chain: Vec<Process>,
    pub group: Option<u32>,
    pub members: Vec<Process>,
    /// What the group's number listed when its leader could not be seen holding it right before
    /// and right after the look (see [`bracketed`]): never signalled, reported while alive.
    pub unproven: Vec<Process>,
    pub signal: i32,
}

/// The processes in `top`'s group, listed by `observe` ([`crate::procinfo::in_group`] outside
/// tests) only within a bracket: `top`, the identified leader, is still the process observed
/// (pid and start time, by `current`) and leads the group right before and right after the
/// listing. A group number is not handed out while its leader lives, so only then is the listing
/// that group's; otherwise `Err` carries what the number listed, which may be another group's.
pub fn bracketed(top: &Process, observe: &dyn Fn(u32) -> Vec<Process>, current: &dyn Fn(&Process) -> Option<Process>) -> Result<Vec<Process>, Vec<Process>> {
    let leads = || current(top).is_some_and(|now| now.group == top.pid);
    let before = leads();
    let members = observe(top.pid);
    if before && leads() { Ok(members) } else { Err(members) }
}

impl Stray {
    pub fn json(&self) -> Value {
        json!({ "service": self.service, "port": self.port, "pid": self.chain[0].pid, "unrecorded": true })
    }
}

pub fn accepting(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(200)).is_ok()
}

/// Whether one signal reached the observed process.
enum Sent {
    Delivered,
    /// The process is gone, or is no longer the one observed.
    Gone,
    /// It could not be signalled in a way bound to it (Linux without pidfd support).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Unbound,
}

/// A pidfd bound to an observed process: opened, then the process checked to still be the one
/// observed, so the descriptor can only ever reach that process. Closed when dropped.
#[cfg(target_os = "linux")]
struct Bound(std::os::fd::OwnedFd);

#[cfg(target_os = "linux")]
impl Bound {
    /// `Ok(None)` when the process is gone or is no longer the one observed; `Err` when this
    /// kernel cannot open a pidfd.
    fn open(p: &Process) -> Result<Option<Bound>, ()> {
        use std::os::fd::FromRawFd;
        // SAFETY: a raw pidfd_open on a pid; a descriptor returned is owned from here on.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, p.pid as libc::pid_t, 0) } as libc::c_int;
        if fd < 0 {
            return match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ESRCH) => Ok(None),
                _ => Err(()),
            };
        }
        // SAFETY: `fd` was just returned by the kernel and is owned by nothing else.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        // The descriptor names whatever had the pid when it was opened: checked to be the
        // observed process after opening, it can only ever signal that one.
        Ok(p.current().map(|_| Bound(fd)))
    }

    fn send(&self, sig: i32) -> Sent {
        use std::os::fd::AsRawFd;
        // SAFETY: a raw pidfd_send_signal through a descriptor this value owns.
        match unsafe { libc::syscall(libc::SYS_pidfd_send_signal, self.0.as_raw_fd(), sig, std::ptr::null::<libc::siginfo_t>(), 0) } {
            0 => Sent::Delivered,
            _ if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) => Sent::Gone,
            _ => Sent::Unbound,
        }
    }
}

#[cfg(target_os = "linux")]
fn signal_process(p: &Process, sig: i32) -> Sent {
    match Bound::open(p) {
        Ok(Some(bound)) => bound.send(sig),
        Ok(None) => Sent::Gone,
        Err(()) => Sent::Unbound,
    }
}

#[cfg(not(target_os = "linux"))]
fn signal_process(p: &Process, sig: i32) -> Sent {
    if p.current().is_none() {
        return Sent::Gone;
    }
    // SAFETY: kill only sends a signal, to the process just observed again (see the module
    // documentation for the gap between that check and this call).
    if unsafe { libc::kill(p.pid as libc::pid_t, sig) } == 0 { Sent::Delivered } else { Sent::Gone }
}

/// Kill `p` while it is in `session`, a session only the caller's own command and what it
/// started can be in (see [`crate::process`]'s deadline runs). On Linux through a pidfd opened
/// before the session is read again: the descriptor names `p`, or what took its pid, and
/// whichever holds the pid when it is read is in the session, so the signal reaches a process
/// of the session or none; without pidfd support nothing is sent. On macOS `p`'s group is
/// killed by its number right after `p` was seen again in the session with that group: a group
/// never spans sessions, and its number, unlike a pid, goes to another process only once that
/// process makes itself a group leader, so the gap between check and signal (see the module
/// documentation) is narrower than for a pid.
pub(crate) fn kill_in_session(p: &Process, session: u32) {
    let within = || crate::procinfo::session(p.pid) == Some(session);
    #[cfg(target_os = "linux")]
    if let Ok(Some(bound)) = Bound::open(p).map(|bound| bound.filter(|_| within())) {
        bound.send(libc::SIGKILL);
    }
    #[cfg(not(target_os = "linux"))]
    // Never 0 or 1 as a group: negated, those would address this group or every process.
    if p.group > 1 && p.current().is_some_and(|now| now.group == p.group) && within() {
        // SAFETY: kill only sends a signal, to the group just observed in the caller's session.
        unsafe { libc::kill(-(p.group as libc::pid_t), libc::SIGKILL) };
    }
}

/// Whether `top`, the recorded leader, is still the process observed and still leads `group`.
fn leads(top: &Process, group: u32) -> bool {
    top.current().is_some_and(|now| now.group == group)
}

/// What one look at a group found: the members accepted (empty when the look was discarded),
/// and whether one of them could not be signalled in a bound way.
#[derive(Default)]
struct GroupLook {
    members: Vec<Process>,
    unbound: bool,
}

/// Send `sig` to the group `stray` names, with the members found by `observe` (the processes in
/// a group, [`crate::procinfo::in_group`] outside tests), only within a bracket: the recorded
/// leader is checked to be the observed process leading the group right before the group is
/// observed and again right after. While that same process is alive before and after, the
/// group number was held all along (a group number is not handed out while its leader lives),
/// and only processes of the leader's own session can be in its group, so what was observed
/// is the service's. If the second check fails, the whole look is discarded: nothing it found
/// is signalled or remembered (a group number freed in between and reused by another session
/// would otherwise hand that session's processes over).
///
/// On Linux each member's pidfd is opened inside the bracket, before the second check, so each
/// descriptor names a member observed while the group was the service's; the members are then
/// signalled through those descriptors, and descriptors of a discarded look are closed unused.
/// On macOS the group is signalled by its number right after the second check.
fn signal_group(stray: &Stray, sig: i32, observe: &dyn Fn(u32) -> Vec<Process>) -> GroupLook {
    let Some(group) = stray.group else { return GroupLook::default() };
    let top = stray.chain.last().expect("a stray has a process");
    if !leads(top, group) {
        return GroupLook::default();
    }
    let members = observe(group);
    #[cfg(target_os = "linux")]
    {
        let mut bound = Vec::new();
        let mut unbound = false;
        for member in &members {
            match Bound::open(member) {
                Ok(Some(fd)) => bound.push(fd),
                Ok(None) => {}
                Err(()) => unbound = true,
            }
        }
        if !leads(top, group) {
            return GroupLook::default();
        }
        for fd in &bound {
            unbound |= matches!(fd.send(sig), Sent::Unbound);
        }
        GroupLook { members, unbound }
    }
    #[cfg(not(target_os = "linux"))]
    {
        if !leads(top, group) {
            return GroupLook::default();
        }
        // SAFETY: kill only sends a signal; the group's leader was just observed again leading
        // it, so the group cannot be another's (see the module documentation for the gap).
        unsafe { libc::kill(-(group as libc::pid_t), sig) };
        GroupLook { members, unbound: false }
    }
}

/// What makes a process found by a rescan the checkout's, weakest last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Known {
    /// Found in a group number nothing shows is still the original group (no process identified
    /// as being in it, by pid and start time, is in it still): never signalled; reported while
    /// it lives, so the stop is not confirmed.
    Unproven,
    /// Only its process group: signalled only while it is still in the group it was found in
    /// (one that left it is no longer known to be the checkout's, and is left running and
    /// reported).
    InGroup,
    /// What it is itself (the checkout's token).
    Itself,
}

impl Known {
    /// Whether `p`, known this way, may be signalled now.
    fn allows(self, p: &Process) -> bool {
        match self {
            Known::Itself => true,
            Known::InGroup => p.current().is_some_and(|now| now.group == p.group),
            Known::Unproven => false,
        }
    }
}

/// A process a rescan found, and what makes it the checkout's.
pub type Found = (Process, Known);

/// Processes found by what they are themselves (the checkout's token), not by their group.
pub fn independent(found: Vec<Process>) -> Vec<Found> {
    found.into_iter().map(|p| (p, Known::Itself)).collect()
}

/// The members of each group in `groups` (a group number and the processes identified as being
/// in it, pid and start time) that `exempt` does not cover, found by `observe` (the processes in
/// a group, [`crate::procinfo::in_group`] outside tests). A group number is never handed out
/// while a process of its group lives, so a look is that group's only when one identified
/// process is still the process observed and in the group both right before and right after
/// it: its members are then [`Known::InGroup`] and identified too. Otherwise the group may have
/// been replaced by another with the same number: its members are [`Known::Unproven`].
pub fn group_members(groups: &[(u32, std::cell::RefCell<Vec<Process>>)], observe: &dyn Fn(u32) -> Vec<Process>, current: &dyn Fn(&Process) -> Option<Process>, exempt: &dyn Fn(&Process) -> bool) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    for (group, identified) in groups {
        let held = || identified.borrow().iter().any(|p| current(p).is_some_and(|now| now.group == *group));
        let before = held();
        let members = observe(*group);
        let known = if before && held() { Known::InGroup } else { Known::Unproven };
        for p in members {
            if exempt(&p) || found.iter().any(|(q, _)| q.same(&p)) {
                continue;
            }
            if known == Known::InGroup && !identified.borrow().iter().any(|q| q.same(&p)) {
                identified.borrow_mut().push(p.clone());
            }
            found.push((p, known));
        }
    }
    found
}

/// How often the processes are searched again (`rescan`) while waiting for them to exit: that
/// search reads every process's environment.
const RESCAN_EVERY: Duration = Duration::from_millis(250);

/// Stop `strays` and `others` (more processes of the checkout, observed): send each stray's
/// group (see [`signal_group`]) and each stray's processes its first signal, `others`
/// `SIGTERM`, then `SIGKILL` what is left once `grace` passes. Every signal is bound to the
/// process observed (see the module documentation); a process whose parent died is still the
/// same process. Members seen in a stray's group are remembered (pid and start time), waited
/// for, and killed one by one even once the leader is gone; they are not signalled one by one
/// while the leader handles its stop signal (a Postgres postmaster stops its backends itself
/// on `SIGINT`): only once their leader exited, with the service's stop signal, and at the
/// `SIGKILL`. `rescan`, when given, finds more of them (a process may start another while it
/// stops), which are stopped alike, and is asked once more before anything is reported gone.
/// Returns what is still alive or still listening, and what could not be signalled in a bound
/// way.
#[cfg(unix)]
pub fn stop(strays: &[Stray], others: &[Process], rescan: Option<&dyn Fn() -> Vec<Found>>, grace: Duration) -> Vec<Value> {
    stop_observing(strays, others, rescan, grace, &crate::procinfo::in_group)
}

/// [`stop`], with the groups' members found by `observe`.
#[cfg(unix)]
fn stop_observing(strays: &[Stray], others: &[Process], rescan: Option<&dyn Fn() -> Vec<Found>>, grace: Duration, observe: &dyn Fn(u32) -> Vec<Process>) -> Vec<Value> {
    // A stray's members are signalled one by one only while still in the group they were
    // observed in (a member that left it is no longer known to be the service's).
    let mut processes: Vec<(Process, i32, Known)> = strays
        .iter()
        .flat_map(|s| s.chain.iter().map(move |p| (p.clone(), s.signal, Known::Itself)).chain(s.members.iter().map(move |p| (p.clone(), s.signal, Known::InGroup))).chain(s.unproven.iter().map(move |p| (p.clone(), s.signal, Known::Unproven))))
        .chain(others.iter().map(|p| (p.clone(), libc::SIGTERM, Known::Itself)))
        .collect();
    // Members remembered from accepted looks at each stray's group, by stray, and whether they
    // were sent the stray's stop signal once its leader was gone.
    let mut remembered: Vec<Vec<Process>> = vec![Vec::new(); strays.len()];
    let mut released = vec![false; strays.len()];
    let mut unbound: Vec<Process> = Vec::new();
    let mark_unbound = |unbound: &mut Vec<Process>, p: &Process| {
        if !unbound.iter().any(|q| q.same(p)) {
            unbound.push(p.clone());
        }
    };
    let send = |processes: &[(Process, i32, Known)], remembered: &mut Vec<Vec<Process>>, unbound: &mut Vec<Process>, kill: bool| {
        for (i, stray) in strays.iter().enumerate() {
            let look = signal_group(stray, if kill { libc::SIGKILL } else { stray.signal }, observe);
            for m in look.members {
                if look.unbound {
                    mark_unbound(unbound, &m);
                }
                if !remembered[i].iter().any(|q| q.same(&m)) {
                    remembered[i].push(m);
                }
            }
        }
        for (p, sig, known) in processes {
            if !known.allows(p) {
                continue;
            }
            if let Sent::Unbound = signal_process(p, if kill { libc::SIGKILL } else { *sig }) {
                mark_unbound(unbound, p);
            }
        }
        if kill {
            for p in remembered.iter().flatten() {
                if let Sent::Unbound = signal_process(p, libc::SIGKILL) {
                    mark_unbound(unbound, p);
                }
            }
        }
    };
    // Members of a stray whose leader exited get the stray's stop signal, once.
    let release = |remembered: &[Vec<Process>], released: &mut [bool], unbound: &mut Vec<Process>| {
        for (i, stray) in strays.iter().enumerate() {
            if released[i] || remembered[i].is_empty() || stray.chain.last().is_some_and(|top| top.current().is_some()) {
                continue;
            }
            released[i] = true;
            for p in &remembered[i] {
                if let Sent::Unbound = signal_process(p, stray.signal) {
                    mark_unbound(unbound, p);
                }
            }
        }
    };
    // Adds what `rescan` finds that is not known yet, signalled at once with the current
    // phase's signal, and says how many that were. One found only by its group is signalled,
    // then and later, only while still in that group; found again by what it is itself, it no
    // longer needs to be.
    let discover = |processes: &mut Vec<(Process, i32, Known)>, remembered: &[Vec<Process>], unbound: &mut Vec<Process>, kill: bool| -> usize {
        let mut found: Vec<Found> = Vec::new();
        for (p, known) in rescan.map(|r| r()).unwrap_or_default() {
            if let Some(entry) = processes.iter_mut().find(|(q, _, _)| q.same(&p)) {
                entry.2 = entry.2.max(known);
            } else if !remembered.iter().flatten().any(|q| q.same(&p)) && !found.iter().any(|(q, _)| q.same(&p)) {
                found.push((p, known));
            }
        }
        for (p, known) in &found {
            if !known.allows(p) {
                continue;
            }
            if let Sent::Unbound = signal_process(p, if kill { libc::SIGKILL } else { libc::SIGTERM }) {
                mark_unbound(unbound, p);
            }
        }
        let n = found.len();
        processes.extend(found.into_iter().map(|(p, known)| (p, libc::SIGTERM, known)));
        n
    };
    let gone = |processes: &[(Process, i32, Known)], remembered: &[Vec<Process>]| {
        strays.iter().all(|s| s.port.map_or(true, |p| !accepting(p)))
            && processes.iter().all(|(p, _, _)| p.current().is_none())
            && remembered.iter().flatten().all(|p| p.current().is_none())
    };
    send(&processes, &mut remembered, &mut unbound, false);
    let deadline = Instant::now() + crate::process::bounded(grace);
    let mut killing = false;
    let mut searched = Instant::now();
    loop {
        release(&remembered, &mut released, &mut unbound);
        if searched.elapsed() >= RESCAN_EVERY {
            discover(&mut processes, &remembered, &mut unbound, killing);
            searched = Instant::now();
        }
        if (gone(&processes, &remembered) && discover(&mut processes, &remembered, &mut unbound, killing) == 0) || !unbound.is_empty() {
            break;
        }
        let now = Instant::now();
        if now >= deadline + crate::process::bounded(Duration::from_secs(2)) {
            break;
        }
        if now >= deadline && !killing {
            killing = true;
            send(&processes, &mut remembered, &mut unbound, true);
        }
        std::thread::sleep(Duration::from_millis(if killing { 50 } else { 20 }));
    }
    let mut left: Vec<Value> = strays.iter().filter(|s| s.port.is_some_and(accepting)).map(Stray::json).collect();
    let still = processes.iter().map(|(p, _, known)| (p, *known)).chain(remembered.iter().flatten().map(|p| (p, Known::InGroup))).filter(|(p, _)| p.current().is_some());
    left.extend(still.map(|(p, known)| {
        let mut v = json!({ "pid": p.pid, "unrecorded": true });
        if unbound.iter().any(|q| q.same(p)) {
            v["reason"] = json!("this kernel cannot signal it through a pidfd, so it was not signalled");
        }
        // Not shown to be the checkout's: never presented as one of its processes.
        let unproven = match known {
            Known::Unproven => Some("found by a process group number that may since name another group; it could not be shown to belong to this checkout, so it was not signalled: check what it is before stopping it"),
            Known::InGroup if !known.allows(p) => Some("it left the process group it was found in, so it can no longer be shown to belong to this checkout and was not signalled: check what it is before stopping it"),
            _ => None,
        };
        if let Some(why) = unproven {
            v["unproven"] = json!(true);
            v["reason"] = json!(why);
        }
        v
    }));
    left
}

/// `stop_unconfirmed` for what [`stop`] left (`left`, non-empty): processes it could not show to
/// be the checkout's (`unproven`) are named as such, never as its own.
pub fn unconfirmed(left: Vec<Value>) -> crate::error::StackError {
    let unproven = left.iter().filter(|d| d["unproven"] == true).count();
    let message = if unproven == left.len() {
        "a process found with this checkout's services could not be shown to belong to it, so it was not signalled"
    } else {
        "a service process the supervisor no longer tracks did not stop"
    };
    let hint = if unproven > 0 {
        "a detail marked `unproven` is a process Stack could not show belongs to this checkout: check what it is before stopping it yourself; then retry `stack down`"
    } else {
        "retry `stack down`; stopping is safe to repeat"
    };
    crate::error::StackError::new("stop_unconfirmed", message).hint(hint).details(left)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::process::{CommandExt, ExitStatusExt};

    fn sleeper(own_group: bool) -> std::process::Child {
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        if own_group {
            command.process_group(0);
        }
        command.spawn().unwrap()
    }

    fn stray_for(leader: &std::process::Child) -> Stray {
        let top = crate::procinfo::process(leader.id()).unwrap();
        Stray { service: "web".into(), port: None, group: Some(top.pid), chain: vec![top], members: Vec::new(), unproven: Vec::new(), signal: libc::SIGTERM }
    }

    /// The leader of a recorded group exits between the check before its group is looked at and
    /// the check after, and what the look returns is another session's process (as when the
    /// group number was reused in between): nothing of that look is ever signalled, then or at
    /// the kill.
    #[test]
    fn nothing_found_in_a_group_whose_leader_vanished_during_the_look_is_signalled() {
        let leader = RefCell::new(sleeper(true));
        let mut bystander = sleeper(true);
        let stray = stray_for(&leader.borrow());
        let bystander_pid = bystander.id();
        let looks = RefCell::new(0);
        let observe = |_: u32| {
            *looks.borrow_mut() += 1;
            let mut l = leader.borrow_mut();
            let _ = l.kill();
            let _ = l.wait();
            vec![crate::procinfo::process(bystander_pid).unwrap()]
        };
        let left = stop_observing(std::slice::from_ref(&stray), &[], None, Duration::from_millis(300), &observe);
        assert_eq!(*looks.borrow(), 1, "the group is looked at only while its leader leads it");
        assert!(left.is_empty(), "{left:?}");
        assert!(bystander.try_wait().unwrap().is_none(), "a process of a discarded look was signalled");
        let _ = bystander.kill();
        let _ = bystander.wait();
    }

    /// A process a rescan found only by its group is signalled only while it is still in that
    /// group: here it ignores SIGTERM, so only the kill can stop it. Recorded in a group it is no
    /// longer in, it is left running and reported; still in it, or found by what it is itself,
    /// it is killed.
    #[test]
    fn a_process_found_by_its_group_is_signalled_only_while_still_in_it() {
        use std::os::unix::process::CommandExt;
        for (moved, known) in [(true, Known::InGroup), (false, Known::InGroup), (true, Known::Itself), (false, Known::Unproven)] {
            let mut command = std::process::Command::new("sleep");
            command.arg("30");
            // SAFETY: signal is async-signal-safe; the child ignores SIGTERM.
            unsafe { command.pre_exec(|| { libc::signal(libc::SIGTERM, libc::SIG_IGN); Ok(()) }) };
            let mut child = command.spawn().unwrap();
            let mut seen = crate::procinfo::process(child.id()).unwrap();
            if moved {
                // As if found in a group it has left since.
                seen.group = seen.group.wrapping_add(1_000_000);
            }
            let rescan = || vec![(seen.clone(), known)];
            let left = stop_observing(&[], &[], Some(&rescan), Duration::from_millis(200), &crate::procinfo::in_group);
            let survived = child.try_wait().unwrap().is_none();
            let spared = known == Known::Unproven || (moved && known == Known::InGroup);
            assert_eq!(survived, spared, "moved {moved}, {known:?}: {left:?}");
            assert_eq!(!left.is_empty(), survived, "{left:?}");
            // What was spared is never presented as the checkout's own.
            assert!(left.iter().all(|d| d["unproven"] == true && d["reason"].as_str().unwrap().contains("check what it is before stopping it")), "{left:?}");
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// A group's members found by number are the original group's only while a process
    /// identified in it is still the same process in it, before and after the look: then they
    /// are `InGroup` (and identified from then on); once every identified process is gone (the
    /// number may have been reused), what the number lists is `Unproven`. Exempt processes are
    /// never returned.
    #[test]
    fn a_group_number_counts_only_while_a_process_identified_in_it_holds_it() {
        let p = |pid: u32, group: u32| Process { pid, parent: 1, group, started: u64::from(pid) * 10 };
        let (anchor, worker, exempt, stranger) = (p(10, 10), p(11, 10), p(12, 10), p(13, 10));
        let groups = vec![(10, std::cell::RefCell::new(vec![anchor.clone()]))];
        let alive = std::cell::RefCell::new(vec![anchor.clone(), worker.clone(), exempt.clone()]);
        let current = |q: &Process| alive.borrow().iter().find(|a| a.same(q)).cloned();
        let observe = |_: u32| alive.borrow().clone();
        let is_exempt = |q: &Process| q.same(&exempt);
        let found = group_members(&groups, &observe, &current, &is_exempt);
        assert_eq!(found, vec![(anchor.clone(), Known::InGroup), (worker.clone(), Known::InGroup)]);
        // The anchor exits; the worker found while anchored now holds the group.
        alive.borrow_mut().retain(|a| !a.same(&anchor));
        assert_eq!(group_members(&groups, &observe, &current, &is_exempt), vec![(worker.clone(), Known::InGroup)]);
        // Every identified process is gone and the number now lists another group's process.
        *alive.borrow_mut() = vec![stranger.clone()];
        assert_eq!(group_members(&groups, &observe, &current, &is_exempt), vec![(stranger, Known::Unproven)]);
    }

    /// The first listing of a stray's group is that group's only while its identified leader
    /// holds it right before and right after: a leader gone (or no longer leading) on either side
    /// makes what the number listed unproven. An unproven member is never signalled, and is
    /// reported as not shown to be the checkout's, while the stray's own process is stopped.
    #[test]
    fn a_first_group_listing_without_its_leader_holding_the_number_is_unproven() {
        let leader = Process { pid: 50, parent: 1, group: 50, started: 500 };
        let stranger = Process { pid: 51, parent: 1, group: 50, started: 510 };
        let observe = |_: u32| vec![stranger.clone()];
        let held = |p: &Process| Some(p.clone());
        assert_eq!(bracketed(&leader, &observe, &held), Ok(vec![stranger.clone()]));
        // Gone right after the listing (the number may have been reused in between).
        let calls = std::cell::Cell::new(0);
        let gone_after = |p: &Process| {
            calls.set(calls.get() + 1);
            (calls.get() == 1).then(|| p.clone())
        };
        assert_eq!(bracketed(&leader, &observe, &gone_after), Err(vec![stranger.clone()]));
        // Gone before it: never held.
        assert_eq!(bracketed(&leader, &observe, &|_| None), Err(vec![stranger.clone()]));
        let left_group = |p: &Process| Some(Process { group: 7, ..p.clone() });
        assert!(bracketed(&leader, &observe, &left_group).is_err());

        let mut own = sleeper(true);
        let mut bystander = sleeper(true);
        let mut stray = stray_for(&own);
        stray.group = None;
        stray.unproven = vec![crate::procinfo::process(bystander.id()).unwrap()];
        let left = stop_observing(std::slice::from_ref(&stray), &[], None, Duration::from_millis(200), &crate::procinfo::in_group);
        assert!(own.wait().unwrap().signal().is_some(), "the stray itself is stopped");
        assert!(bystander.try_wait().unwrap().is_none(), "an unproven member was signalled");
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!((left[0]["pid"].as_u64(), left[0]["unproven"].as_bool()), (Some(u64::from(bystander.id())), Some(true)), "{left:?}");
        let _ = bystander.kill();
        let _ = bystander.wait();
    }

    /// The same look with the leader alive throughout is accepted: what it found is stopped
    /// (here a process the observer reports as a member), so the test above fails only because
    /// of the bracket.
    #[test]
    fn what_a_group_look_finds_while_its_leader_holds_the_group_is_stopped() {
        let mut leader = sleeper(true);
        let mut member = sleeper(true);
        let stray = stray_for(&leader);
        let member_pid = member.id();
        let observe = |_: u32| crate::procinfo::process(member_pid).into_iter().collect::<Vec<_>>();
        let reaper = std::thread::spawn(move || {
            let _ = leader.wait();
            member.wait().unwrap()
        });
        let left = stop_observing(std::slice::from_ref(&stray), &[], None, Duration::from_secs(5), &observe);
        assert!(left.is_empty(), "{left:?}");
        assert!(reaper.join().unwrap().signal().is_some());
    }
}

/// Without Unix signals nothing is stopped, so everything is reported as surviving.
#[cfg(not(unix))]
pub fn stop(strays: &[Stray], others: &[Process], _rescan: Option<&dyn Fn() -> Vec<Found>>, _grace: Duration) -> Vec<Value> {
    strays.iter().map(Stray::json).chain(others.iter().map(|p| json!({ "pid": p.pid, "unrecorded": true }))).collect()
}

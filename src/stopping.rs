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
    pub signal: i32,
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
pub fn stop(strays: &[Stray], others: &[Process], rescan: Option<&dyn Fn() -> Vec<Process>>, grace: Duration) -> Vec<Value> {
    stop_observing(strays, others, rescan, grace, &crate::procinfo::in_group)
}

/// [`stop`], with the groups' members found by `observe`.
#[cfg(unix)]
fn stop_observing(strays: &[Stray], others: &[Process], rescan: Option<&dyn Fn() -> Vec<Process>>, grace: Duration, observe: &dyn Fn(u32) -> Vec<Process>) -> Vec<Value> {
    // A stray's members are signalled one by one only while still in the group they were
    // observed in (a member that left it is no longer known to be the service's).
    let mut processes: Vec<(Process, i32, bool)> = strays
        .iter()
        .flat_map(|s| s.chain.iter().map(move |p| (p.clone(), s.signal, false)).chain(s.members.iter().map(move |p| (p.clone(), s.signal, true))))
        .chain(others.iter().map(|p| (p.clone(), libc::SIGTERM, false)))
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
    let send = |processes: &[(Process, i32, bool)], remembered: &mut Vec<Vec<Process>>, unbound: &mut Vec<Process>, kill: bool| {
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
        for (p, sig, grouped) in processes {
            if *grouped && !p.current().is_some_and(|now| now.group == p.group) {
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
    // phase's signal, and says how many that were.
    let discover = |processes: &mut Vec<(Process, i32, bool)>, remembered: &[Vec<Process>], unbound: &mut Vec<Process>, kill: bool| -> usize {
        let known = |p: &Process, processes: &[(Process, i32, bool)]| processes.iter().any(|(q, _, _)| q.same(p)) || remembered.iter().flatten().any(|q| q.same(p));
        let found: Vec<Process> = rescan.map(|r| r()).unwrap_or_default().into_iter().filter(|p| !known(p, processes)).collect();
        for p in &found {
            if let Sent::Unbound = signal_process(p, if kill { libc::SIGKILL } else { libc::SIGTERM }) {
                mark_unbound(unbound, p);
            }
        }
        let n = found.len();
        processes.extend(found.into_iter().map(|p| (p, libc::SIGTERM, false)));
        n
    };
    let gone = |processes: &[(Process, i32, bool)], remembered: &[Vec<Process>]| {
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
    let still = processes.iter().map(|(p, _, _)| p).chain(remembered.iter().flatten()).filter(|p| p.current().is_some());
    left.extend(still.map(|p| {
        let mut v = json!({ "pid": p.pid, "unrecorded": true });
        if unbound.iter().any(|q| q.same(p)) {
            v["reason"] = json!("this kernel cannot signal it through a pidfd, so it was not signalled");
        }
        v
    }));
    left
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
        Stray { service: "web".into(), port: None, group: Some(top.pid), chain: vec![top], members: Vec::new(), signal: libc::SIGTERM }
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
pub fn stop(strays: &[Stray], others: &[Process], _rescan: Option<&dyn Fn() -> Vec<Process>>, _grace: Duration) -> Vec<Value> {
    strays.iter().map(Stray::json).chain(others.iter().map(|p| json!({ "pid": p.pid, "unrecorded": true }))).collect()
}

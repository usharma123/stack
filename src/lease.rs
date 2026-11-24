//! How a session's lease is reported (the same structured summary from `status`, `up` and
//! `ps`, so an agent need not parse the free-text `lease_expired`), how `--owner-pid` names
//! its owner, and how that owner is told from a later process that reuses its PID.

use crate::error::{Result, StackError};
use crate::procinfo::{self, Named};
use crate::session::{Lease, MAX_OWNER_PID};
use crate::state::{now, pid_alive};
use serde::{Deserialize, Serialize};

/// The owner process of a lease as observed when the lease named it: its start time (see
/// [`procinfo::Process::started`]) and the boot it ran in (see [`procinfo::boot_id`]), as
/// [`crate::supervisor::Proc`] records a service, and the PID namespace of the Stack process
/// that observed it, in which alone its PID means it. Each is `None` when it could not be
/// observed. A lease an earlier Stack recorded has none of this: its owner is a bare PID (see
/// [`owner_state`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_ns: Option<u64>,
}

impl OwnerIdentity {
    /// The process `pid` now, as far as it can be observed.
    pub fn observe(pid: u32) -> Self {
        Self::observe_with(pid, &Here)
    }

    pub(crate) fn observe_with(pid: u32, here: &dyn Observe) -> Self {
        Self { started: here.started(pid), boot: here.boot(), pid_ns: here.pid_ns() }
    }
}

/// Whether a lease's owner still runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerState {
    Alive,
    /// It exited (or the machine restarted since): why. The lease has run out.
    Exited(String),
    /// Whether it runs cannot be told from here: why. The lease is kept, as for a live owner;
    /// a TTL still runs out.
    Unknown(String),
}

/// What this process can observe of others, for tests to stand in for.
pub(crate) trait Observe {
    fn boot(&self) -> Option<String>;
    fn pid_ns(&self) -> Option<u64>;
    /// Whether PIDs are per namespace here (see [`procinfo::PID_NAMESPACES`]).
    fn namespaced(&self) -> bool;
    /// The start time of the process `pid` is now, when it can be read.
    fn started(&self, pid: u32) -> Option<u64>;
    /// Whether `pid` names any process (including one this process may not inspect).
    fn alive(&self, pid: u32) -> bool;
}

/// This machine, as this process sees it.
pub(crate) struct Here;

impl Observe for Here {
    fn boot(&self) -> Option<String> {
        procinfo::boot_id()
    }
    fn pid_ns(&self) -> Option<u64> {
        procinfo::pid_namespace()
    }
    fn namespaced(&self) -> bool {
        procinfo::PID_NAMESPACES
    }
    fn started(&self, pid: u32) -> Option<u64> {
        procinfo::process(pid).map(|p| p.started)
    }
    fn alive(&self, pid: u32) -> bool {
        pid_alive(pid)
    }
}

/// Whether the owner `pid` of a lease, observed as `identity` when the lease named it, still runs.
///
/// - A lease an earlier Stack recorded (no `identity`) names its owner by PID alone: alive while
///   any process has that PID, as it always was.
/// - Recorded in another boot: exited.
/// - Where PIDs are per namespace, recorded from another namespace, or one that cannot be read
///   (then or now): unknown, since the PID may mean another process here.
/// - Otherwise the process now at `pid` is the owner only with the recorded start time: another
///   one is a reused PID, so the owner exited. No process at all: exited. A process whose start
///   time cannot be read, or an owner whose start time could not be read when recorded, while
///   something runs as `pid`: unknown.
pub fn owner_state(pid: u32, identity: Option<&OwnerIdentity>) -> OwnerState {
    owner_state_with(pid, identity, &Here)
}

pub(crate) fn owner_state_with(pid: u32, identity: Option<&OwnerIdentity>, here: &dyn Observe) -> OwnerState {
    let exited = || OwnerState::Exited(format!("owner process {pid} exited"));
    let Some(identity) = identity else {
        return if here.alive(pid) { OwnerState::Alive } else { exited() };
    };
    if let (Some(recorded), Some(now)) = (&identity.boot, here.boot()) {
        if *recorded != now {
            return OwnerState::Exited(format!("owner process {pid} exited: the machine restarted since the lease named it"));
        }
    }
    if here.namespaced() && (identity.pid_ns.is_none() || identity.pid_ns != here.pid_ns()) {
        return OwnerState::Unknown(format!(
            "owner process {pid} was named from another PID namespace than this one, or one that cannot be read, so PID {pid} \
             here may be another process"
        ));
    }
    match (here.started(pid), identity.started) {
        (Some(now), Some(recorded)) if now == recorded => OwnerState::Alive,
        (Some(_), Some(_)) => OwnerState::Exited(format!("owner process {pid} exited: PID {pid} now names another process")),
        (Some(_), None) => OwnerState::Unknown(format!(
            "the start time of owner process {pid} could not be read when the lease named it, so the process now at PID {pid} \
             cannot be told from it"
        )),
        (None, _) if !here.alive(pid) => exited(),
        (None, _) => OwnerState::Unknown(format!("PID {pid} runs, but its start time cannot be read to tell whether it is the owner")),
    }
}

/// What `--owner-pid` (MCP `owner_pid` or `owner_ancestor`) names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    Pid(u32),
    /// The nearest ancestor of this stack process with this command name.
    Ancestor(String),
}

impl Owner {
    /// `<pid>` or `ancestor:<name>`.
    pub fn parse(value: &str) -> Result<Self> {
        if let Some(name) = value.strip_prefix("ancestor:") {
            return Self::ancestor(name);
        }
        value
            .parse::<u64>()
            .map_err(|_| {
                StackError::new("usage", format!("--owner-pid {value:?} is neither a process ID nor ancestor:<name>"))
                    .hint("pass a process ID, or ancestor:<command name> such as ancestor:claude")
            })
            .and_then(crate::session::owner_pid)
            .map(Self::Pid)
    }

    pub fn ancestor(name: &str) -> Result<Self> {
        if name.is_empty() || name.contains('/') || name.chars().any(char::is_whitespace) {
            return Err(StackError::new("usage", format!("ancestor name {name:?} must be a command name, such as claude"))
                .hint("give the runner's command name as `ps -o comm` shows it, without a path"));
        }
        Ok(Self::Ancestor(name.to_string()))
    }

    /// The owner's process ID, walking this process's parents for an ancestor.
    pub fn resolve(&self) -> Result<u32> {
        match self {
            Self::Pid(pid) => Ok(*pid),
            Self::Ancestor(name) => resolve_ancestor(name, &procinfo::ancestors()),
        }
    }
}

/// The nearest process in `chain` named `name`, else `owner_not_found` listing the chain.
pub fn resolve_ancestor(name: &str, chain: &[Named]) -> Result<u32> {
    if let Some(found) = chain.iter().find(|p| p.named(name) && (1..=MAX_OWNER_PID).contains(&p.pid)) {
        return Ok(found.pid);
    }
    let listed: Vec<String> = chain
        .iter()
        .map(|p| match &p.argv0 {
            Some(argv0) => format!("{} {} ({argv0})", p.pid, p.comm),
            None => format!("{} {}", p.pid, p.comm),
        })
        .collect();
    let shown = if listed.is_empty() { "none could be read".to_string() } else { listed.join(" <- ") };
    Err(StackError::new("owner_not_found", format!("no ancestor of this stack process is named '{name}'; ancestors, nearest first: {shown}"))
        .hint("pick a name from that chain (`--owner-pid ancestor:<name>`), pass the runner's process ID with `--owner-pid <pid>`, or use `--ttl 30m` instead")
        .details(chain.iter().map(|p| serde_json::to_value(p).expect("process names serialize")).collect()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseSummary {
    /// `ttl`, `owner_pid`, `ttl+owner_pid`, or `none` (lives until `stack down`, or no session).
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    /// Whether the owner process is alive now: absent when that cannot be told (see
    /// `owner_unknown`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_alive: Option<bool>,
    /// How the owner is known: `pid+start_time+boot`, or `pid` for a lease an earlier Stack
    /// recorded (alive while any process has that PID).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_identity: Option<&'static str>,
    /// Why whether the owner runs cannot be told; the lease is kept meanwhile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_unknown: Option<String>,
    /// When it was last renewed (Unix seconds; 0 without a lease).
    pub renewed_at: u64,
    /// When the TTL runs out unless renewed (Unix seconds); absent without a TTL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// The lease ran out: `stack gc`, or the next `stack up` anywhere, reclaims the session.
    pub expired: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expired_reason: Option<String>,
}

impl LeaseSummary {
    pub fn none() -> Self {
        Self {
            kind: "none",
            ttl_secs: None,
            owner_pid: None,
            owner_alive: None,
            owner_identity: None,
            owner_unknown: None,
            renewed_at: 0,
            expires_at: None,
            expired: false,
            expired_reason: None,
        }
    }

    /// `lease` as it stands now.
    pub fn of(lease: Option<&Lease>) -> Self {
        Self::at(lease, now())
    }

    fn at(lease: Option<&Lease>, now: u64) -> Self {
        let Some(lease) = lease else { return Self::none() };
        let kind = match (lease.ttl_secs, lease.owner_pid) {
            (Some(_), Some(_)) => "ttl+owner_pid",
            (Some(_), None) => "ttl",
            (None, Some(_)) => "owner_pid",
            (None, None) => "none",
        };
        let reason = lease.expired(now);
        let owner = lease.owner_state();
        Self {
            kind,
            ttl_secs: lease.ttl_secs,
            owner_pid: lease.owner_pid,
            owner_alive: match &owner {
                Some(OwnerState::Alive) => Some(true),
                Some(OwnerState::Exited(_)) => Some(false),
                Some(OwnerState::Unknown(_)) | None => None,
            },
            owner_identity: lease.owner_pid.map(|_| if lease.owner.is_some() { "pid+start_time+boot" } else { "pid" }),
            owner_unknown: match owner {
                Some(OwnerState::Unknown(why)) => Some(why),
                _ => None,
            },
            renewed_at: lease.renewed_at,
            expires_at: lease.ttl_secs.map(|ttl| lease.renewed_at.saturating_add(ttl)),
            expired: reason.is_some(),
            expired_reason: reason,
        }
    }

    /// One line for people, or `None` without a lease.
    pub fn line(&self) -> Option<String> {
        let ttl = self.ttl_secs.map(|t| match self.expires_at {
            Some(at) => format!("ttl {t}s, {}s left", at.saturating_sub(now())),
            None => format!("ttl {t}s"),
        });
        let owner = self.owner_pid.map(|p| {
            let state = match self.owner_alive {
                Some(true) => "alive",
                Some(false) => "exited",
                None => "cannot be told",
            };
            format!("owner pid {p} ({state})")
        });
        let parts: Vec<String> = ttl.into_iter().chain(owner).collect();
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_kinds_and_expiry_are_reported() {
        assert_eq!(LeaseSummary::at(None, 0), LeaseSummary::none());
        let ttl = Lease { ttl_secs: Some(60), owner_pid: None, owner: None, renewed_at: 1000 };
        let info = LeaseSummary::at(Some(&ttl), 1030);
        assert_eq!((info.kind, info.expires_at, info.expired), ("ttl", Some(1060), false));
        assert!(LeaseSummary::at(Some(&ttl), 1061).expired);
        let owned = Lease { ttl_secs: None, owner_pid: Some(std::process::id()), owner: None, renewed_at: 1000 };
        let info = LeaseSummary::at(Some(&owned), 99_999);
        assert_eq!((info.kind, info.owner_alive, info.expired, info.expires_at), ("owner_pid", Some(true), false, None));
        assert_eq!(info.line().as_deref(), Some(format!("owner pid {} (alive)", std::process::id()).as_str()));
        let both = Lease { ttl_secs: Some(1), ..owned };
        assert_eq!(LeaseSummary::at(Some(&both), 0).kind, "ttl+owner_pid");
        assert_eq!(LeaseSummary::none().line(), None);
    }

    /// A machine as a test says it is: this boot and PID namespace, whether PIDs are per
    /// namespace, and the start time of each running PID (`None`: it runs but cannot be read).
    struct Seen {
        boot: Option<&'static str>,
        pid_ns: Option<u64>,
        namespaced: bool,
        running: Vec<(u32, Option<u64>)>,
    }

    impl Observe for Seen {
        fn boot(&self) -> Option<String> {
            self.boot.map(String::from)
        }
        fn pid_ns(&self) -> Option<u64> {
            self.pid_ns
        }
        fn namespaced(&self) -> bool {
            self.namespaced
        }
        fn started(&self, pid: u32) -> Option<u64> {
            self.running.iter().find(|(p, _)| *p == pid).and_then(|(_, s)| *s)
        }
        fn alive(&self, pid: u32) -> bool {
            self.running.iter().any(|(p, _)| *p == pid)
        }
    }

    fn linux(running: Vec<(u32, Option<u64>)>) -> Seen {
        Seen { boot: Some("boot-a"), pid_ns: Some(4026531836), namespaced: true, running }
    }

    fn exited(state: OwnerState) -> String {
        match state {
            OwnerState::Exited(why) => why,
            other => panic!("{other:?}"),
        }
    }

    fn unknown(state: OwnerState) -> String {
        match state {
            OwnerState::Unknown(why) => why,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_owner_is_the_process_it_was_and_a_reused_pid_is_not_it() {
        let here = linux(vec![(42, Some(1000))]);
        let owner = OwnerIdentity::observe_with(42, &here);
        assert_eq!(owner, OwnerIdentity { started: Some(1000), boot: Some("boot-a".into()), pid_ns: Some(4026531836) });
        assert_eq!(owner_state_with(42, Some(&owner), &here), OwnerState::Alive);
        // The owner exited and its PID went to a later process.
        let reused = linux(vec![(42, Some(2000))]);
        assert!(exited(owner_state_with(42, Some(&owner), &reused)).contains("now names another process"));
        // Nothing runs as 42 any more.
        assert_eq!(exited(owner_state_with(42, Some(&owner), &linux(vec![]))), "owner process 42 exited");
    }

    #[test]
    fn an_owner_recorded_in_another_boot_has_exited_whatever_has_its_pid() {
        let owner = OwnerIdentity { started: Some(1000), boot: Some("boot-a".into()), pid_ns: Some(4026531836) };
        // Linux start times count from boot: the same PID and start time after a reboot is
        // another process.
        let rebooted = Seen { boot: Some("boot-b"), ..linux(vec![(42, Some(1000))]) };
        assert!(exited(owner_state_with(42, Some(&owner), &rebooted)).contains("restarted"));
        // A boot that cannot be read, then or now, is not taken for another one.
        let unread = Seen { boot: None, ..linux(vec![(42, Some(1000))]) };
        assert_eq!(owner_state_with(42, Some(&owner), &unread), OwnerState::Alive);
    }

    #[test]
    fn an_owner_named_from_another_pid_namespace_cannot_be_told_and_keeps_its_lease() {
        let owner = OwnerIdentity { started: Some(1000), boot: Some("boot-a".into()), pid_ns: Some(4026531836) };
        // Another namespace: its PID 42 may be another process here, even one that exited or
        // never ran, so neither alive nor exited.
        for running in [vec![(42, Some(1000))], vec![(42, Some(7))], vec![]] {
            let other = Seen { pid_ns: Some(4026532000), ..linux(running) };
            assert!(unknown(owner_state_with(42, Some(&owner), &other)).contains("another PID namespace"));
        }
        // A namespace that cannot be read, now or when recorded.
        let unread = Seen { pid_ns: None, ..linux(vec![(42, Some(1000))]) };
        assert!(matches!(owner_state_with(42, Some(&owner), &unread), OwnerState::Unknown(_)));
        let recorded_unread = OwnerIdentity { pid_ns: None, ..owner.clone() };
        assert!(matches!(owner_state_with(42, Some(&recorded_unread), &linux(vec![(42, Some(1000))])), OwnerState::Unknown(_)));
        // Another boot still decides: no process of an earlier boot runs now.
        let rebooted = Seen { boot: Some("boot-b"), pid_ns: Some(4026532000), ..linux(vec![]) };
        assert!(matches!(owner_state_with(42, Some(&owner), &rebooted), OwnerState::Exited(_)));
        // Where PIDs are not per namespace (macOS), none is recorded or needed.
        let mac = Seen { boot: Some("boot-a"), pid_ns: None, namespaced: false, running: vec![(42, Some(1000))] };
        let on_mac = OwnerIdentity::observe_with(42, &mac);
        assert_eq!(on_mac.pid_ns, None);
        assert_eq!(owner_state_with(42, Some(&on_mac), &mac), OwnerState::Alive);
    }

    #[test]
    fn an_owner_whose_start_time_cannot_be_read_is_never_taken_for_exited_while_its_pid_runs() {
        // Unreadable when the lease named it: a process at the PID cannot be told from it.
        let unobserved = OwnerIdentity::observe_with(42, &linux(vec![(42, None)]));
        assert_eq!(unobserved.started, None);
        assert!(unknown(owner_state_with(42, Some(&unobserved), &linux(vec![(42, Some(1000))]))).contains("could not be read when the lease named it"));
        assert!(matches!(owner_state_with(42, Some(&unobserved), &linux(vec![])), OwnerState::Exited(_)), "nothing runs as it: exited");
        // Readable then, not now (another user's process, say).
        let owner = OwnerIdentity { started: Some(1000), boot: Some("boot-a".into()), pid_ns: Some(4026531836) };
        assert!(unknown(owner_state_with(42, Some(&owner), &linux(vec![(42, None)]))).contains("cannot be read"));
    }

    /// A lease an earlier Stack recorded names a bare PID: alive while anything has it.
    #[test]
    fn a_legacy_owner_is_a_bare_pid() {
        assert_eq!(owner_state_with(42, None, &linux(vec![(42, Some(2000))])), OwnerState::Alive);
        assert_eq!(exited(owner_state_with(42, None, &linux(vec![]))), "owner process 42 exited");
        let legacy: Lease = serde_json::from_str(r#"{"owner_pid": 42, "renewed_at": 5}"#).unwrap();
        assert_eq!((legacy.owner_pid, legacy.owner.as_ref()), (Some(42), None));
        let summary = LeaseSummary::at(Some(&Lease { owner_pid: Some(std::process::id()), ..legacy }), 10);
        assert_eq!((summary.owner_identity, summary.owner_alive), (Some("pid"), Some(true)));
    }

    /// On this machine: this process is its own owner, a child that exited is not, and a lease
    /// binding survives being written and read back.
    #[test]
    fn a_lease_bound_to_a_process_on_this_machine() {
        let me = std::process::id();
        let lease = Lease { ttl_secs: None, owner_pid: Some(me), owner: Some(OwnerIdentity::observe(me)), renewed_at: 1 };
        let lease: Lease = serde_json::from_str(&serde_json::to_string(&lease).unwrap()).unwrap();
        assert_eq!(lease.owner_state(), Some(OwnerState::Alive));
        assert!(lease.expired(u64::MAX).is_none() && !lease.owner_exited());
        let summary = LeaseSummary::at(Some(&lease), 10);
        assert_eq!((summary.owner_identity, summary.owner_alive, summary.owner_unknown), (Some("pid+start_time+boot"), Some(true), None));
        // Its identity with another start time: what a reused PID looks like.
        let reused = Lease { owner: lease.owner.clone().map(|o| OwnerIdentity { started: o.started.map(|s| s + 1), ..o }), ..lease.clone() };
        assert!(reused.owner_exited());
        assert!(reused.expired(2).unwrap().contains("now names another process"));
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let bound = OwnerIdentity::observe(child.id());
        child.wait().unwrap();
        // Exited, whether its PID is free now or already another process's.
        let gone = Lease { owner_pid: Some(child.id()), owner: Some(bound), ..lease };
        assert!(gone.owner_exited(), "{:?}", gone.owner_state());
    }

    #[test]
    fn owner_values_are_pids_or_ancestor_names() {
        assert_eq!(Owner::parse("42").unwrap(), Owner::Pid(42));
        assert_eq!(Owner::parse("ancestor:claude").unwrap(), Owner::Ancestor("claude".into()));
        for bad in ["0", "2147483648", "-1", "abc", "ancestor:", "ancestor:a/b", "ancestor:a b"] {
            assert_eq!(Owner::parse(bad).unwrap_err().code, "usage", "{bad}");
        }
    }

    #[test]
    fn the_nearest_matching_ancestor_wins_and_a_miss_lists_the_chain() {
        let p = |pid, comm: &str, argv0: Option<&str>| Named { pid, parent: pid - 1, comm: comm.into(), argv0: argv0.map(String::from) };
        let chain = [p(30, "zsh", None), p(20, "node", Some("claude")), p(10, "claude", None), p(2, "launchd", None)];
        assert_eq!(resolve_ancestor("claude", &chain).unwrap(), 20);
        assert_eq!(resolve_ancestor("zsh", &chain).unwrap(), 30);
        let e = resolve_ancestor("codex", &chain).unwrap_err();
        assert_eq!(e.code, "owner_not_found");
        assert!(e.message.contains("30 zsh <- 20 node (claude) <- 10 claude <- 2 launchd"), "{}", e.message);
        assert!(e.hint.unwrap().contains("--ttl"));
        assert_eq!(e.details.len(), 4);
    }
}

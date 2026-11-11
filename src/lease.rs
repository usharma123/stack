//! How a session's lease is reported (the same structured summary from `status`, `up` and
//! `ps`, so an agent need not parse the free-text `lease_expired`), and how `--owner-pid` names
//! its owner.

use crate::error::{Result, StackError};
use crate::procinfo::{self, Named};
use crate::session::{Lease, MAX_OWNER_PID};
use crate::state::{now, pid_alive};
use serde::Serialize;

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
    /// Whether the owner process is alive now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_alive: Option<bool>,
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
        Self { kind: "none", ttl_secs: None, owner_pid: None, owner_alive: None, renewed_at: 0, expires_at: None, expired: false, expired_reason: None }
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
        Self {
            kind,
            ttl_secs: lease.ttl_secs,
            owner_pid: lease.owner_pid,
            owner_alive: lease.owner_pid.map(pid_alive),
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
        let owner = self.owner_pid.map(|p| format!("owner pid {p} ({})", if self.owner_alive == Some(true) { "alive" } else { "exited" }));
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
        let ttl = Lease { ttl_secs: Some(60), owner_pid: None, renewed_at: 1000 };
        let info = LeaseSummary::at(Some(&ttl), 1030);
        assert_eq!((info.kind, info.expires_at, info.expired), ("ttl", Some(1060), false));
        assert!(LeaseSummary::at(Some(&ttl), 1061).expired);
        let owned = Lease { ttl_secs: None, owner_pid: Some(std::process::id()), renewed_at: 1000 };
        let info = LeaseSummary::at(Some(&owned), 99_999);
        assert_eq!((info.kind, info.owner_alive, info.expired, info.expires_at), ("owner_pid", Some(true), false, None));
        assert_eq!(info.line().as_deref(), Some(format!("owner pid {} (alive)", std::process::id()).as_str()));
        let both = Lease { ttl_secs: Some(1), ..owned };
        assert_eq!(LeaseSummary::at(Some(&both), 0).kind, "ttl+owner_pid");
        assert_eq!(LeaseSummary::none().line(), None);
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

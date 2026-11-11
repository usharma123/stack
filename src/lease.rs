//! How a session's lease is reported: the same structured summary from `status`, `up` and `ps`,
//! so an agent need not parse the free-text `lease_expired`.

use crate::session::Lease;
use crate::state::{now, pid_alive};
use serde::Serialize;

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
}

//! Instance tokens for services with identity probes.
//!
//! Each checkout's service gets a random token, kept in machine state like its ports. The
//! service receives it in its environment and a probe must read it back from the live service:
//! a healthy server belonging to another checkout, or to nothing stack started, cannot answer
//! with it. Tokens tell instances apart; they are not credentials.

use crate::error::{io_error, Result};
use crate::hash::sha256_hex;
use crate::state::{read_json, write_json, FileLock};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    tokens: Vec<Token>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Token {
    project: PathBuf,
    service: String,
    token: String,
}

fn paths(state: &Path) -> (PathBuf, PathBuf) {
    (state.join("identities.json"), state.join("identities.lock"))
}

/// The token of each named service, creating missing ones. Tokens of services no longer named
/// and of deleted projects are dropped.
pub fn assign(state: &Path, project: &Path, services: &[String]) -> Result<IndexMap<String, String>> {
    let (file, lock) = paths(state);
    let _guard = FileLock::acquire(&lock)?;
    let mut reg: Registry = read_json(&file)?;
    let before = reg.tokens.len();
    reg.tokens.retain(|t| t.project.exists() && (t.project != project || services.contains(&t.service)));
    let mut changed = reg.tokens.len() != before;
    let mut out = IndexMap::new();
    for service in services {
        let existing = reg.tokens.iter().find(|t| t.project == project && t.service == *service);
        let token = match existing {
            Some(t) => t.token.clone(),
            None => {
                let token = random_token()?;
                reg.tokens.push(Token { project: project.to_path_buf(), service: service.clone(), token: token.clone() });
                changed = true;
                token
            }
        };
        out.insert(service.clone(), token);
    }
    if changed {
        write_json(&file, &reg)?;
    }
    Ok(out)
}

/// Existing tokens, without creating any.
pub fn lookup(state: &Path, project: &Path) -> Result<IndexMap<String, String>> {
    let reg: Registry = read_json(&paths(state).0)?;
    Ok(reg
        .tokens
        .into_iter()
        .filter(|t| t.project == project)
        .map(|t| (t.service, t.token))
        .collect())
}

fn random_token() -> Result<String> {
    let mut seed = Vec::with_capacity(64);
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut bytes = [0u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .map_err(|e| io_error("/dev/urandom", e))?;
        seed.extend_from_slice(&bytes);
    }
    seed.extend_from_slice(format!("{:?}{}", std::time::SystemTime::now(), std::process::id()).as_bytes());
    Ok(format!("stack-{}", &sha256_hex(&seed)[..32]))
}

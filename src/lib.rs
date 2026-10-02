//! stack: compose reusable development bundles and run them through existing providers.

pub mod compose;
pub mod error;
pub mod git;
pub mod hash;
pub mod lock;
pub mod manifest;
pub mod mcp;
pub mod oci;
pub mod ports;
pub mod project;
pub mod provider;
pub mod session;
pub mod source;
pub mod state;

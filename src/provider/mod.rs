//! Providers turn a composed stack into something an existing tool can run.
//! Bundles never name a provider; swapping one must not change what a bundle means.

pub mod mise;
pub mod scratch;
pub mod task_config;

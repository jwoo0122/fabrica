//! Iteration 0 stub for the SDUI JSON wire format.
//!
//! Real schema arrives in a later iteration — see CLAUDE.md §"Open decisions".
//! This crate currently only proves that `serde` + `serde_json` are reachable.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePlaceholder {
    pub version: u32,
}

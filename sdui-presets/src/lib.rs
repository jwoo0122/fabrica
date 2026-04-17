//! Iteration 0 stub for the preset library resolver.
//!
//! Real resolver (preset library JSON loader + `PresetRef` expansion + cycle
//! detection) is out of scope for Iteration 0. See CLAUDE.md decision §8.

pub fn crate_name() -> &'static str {
    "sdui-presets"
}

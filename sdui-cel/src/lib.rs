//! Iteration 0 stub for the CEL expression engine wrapper.
//!
//! The future `ExpressionEngine` trait — with a `cel-interpreter`-backed
//! default impl — is documented in CLAUDE.md but deferred. This crate only
//! exists in Iteration 0 to prove the dependency resolves.

/// Placeholder trait; real signatures land when CEL evaluation is needed.
pub trait ExpressionEngine {
    fn name(&self) -> &'static str;
}

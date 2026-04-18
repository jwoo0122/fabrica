//! CEL expression engine for SDUI runtime.
//!
//! Iteration 5 exposes the smallest viable trait (`compile` + `eval_bool`)
//! backed by `cel-interpreter` 0.10. The evaluation environment is empty —
//! only literal and built-in operator expressions are supported. Variable
//! binding is deferred to a later iteration.
//!
//! References:
//!   - `cel-interpreter::Program` docs: <https://docs.rs/cel-interpreter/0.10/cel_interpreter/struct.Program.html>
//!   - `cel-interpreter::Context::default`: <https://docs.rs/cel-interpreter/0.10/cel_interpreter/context/enum.Context.html>
//!   - CEL langdef (bool strictness): <https://github.com/google/cel-spec/blob/master/doc/langdef.md>

use cel_interpreter::objects::Value;
use cel_interpreter::{Context, Program};

/// Smallest viable surface for a runtime expression engine.
///
/// The pivot path to JSON Logic or a custom DSL (CLAUDE.md §"Expression
/// language: CEL") keeps this trait free of `cel-interpreter` types.
pub trait ExpressionEngine {
    /// Parse `src` into a compiled, reusable expression.
    fn compile(&self, src: &str) -> Result<CompiledExpr, ExprError>;

    /// Evaluate `expr` and require a boolean result.
    fn eval_bool(&self, expr: &CompiledExpr) -> Result<bool, ExprError>;
}

/// A parsed CEL program, opaque to callers outside this crate.
pub struct CompiledExpr(pub(crate) Program);

impl std::fmt::Debug for CompiledExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CompiledExpr").field(&self.0).finish()
    }
}

/// Errors that can arise from parsing or evaluating an expression.
#[derive(Debug, thiserror::Error)]
pub enum ExprError {
    /// Source text failed to parse.
    #[error("expression parse failed: {0}")]
    ParseFailed(String),
    /// Expression executed but raised a runtime error.
    #[error("expression evaluation failed: {0}")]
    EvalFailed(String),
    /// Expression produced a non-boolean value in a boolean context.
    #[error("expression did not evaluate to bool (got {got})")]
    NotBool {
        /// Stable CEL-value variant name (e.g. `"Int"`, `"String"`).
        got: String,
    },
}

/// Default `ExpressionEngine` implementation backed by `cel-interpreter`.
pub struct CelEngine;

impl CelEngine {
    /// Construct a new engine. No heap allocation; built-in operators are
    /// installed lazily per-evaluation via `Context::default`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CelEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ExpressionEngine for CelEngine {
    fn compile(&self, src: &str) -> Result<CompiledExpr, ExprError> {
        // `cel-parser` 0.10.1 panics on some malformed inputs (e.g. "1 +")
        // via `antlr4rust`'s `unreachable!` path. Catch and surface as a
        // structured error so scene loading never aborts the runtime.
        let src_owned = src.to_string();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Program::compile(&src_owned)
        }));
        match result {
            Ok(Ok(p)) => Ok(CompiledExpr(p)),
            Ok(Err(e)) => Err(ExprError::ParseFailed(e.to_string())),
            Err(_) => Err(ExprError::ParseFailed(format!(
                "parser panicked on input {src:?}"
            ))),
        }
    }

    fn eval_bool(&self, expr: &CompiledExpr) -> Result<bool, ExprError> {
        let ctx = Context::default();
        match expr.0.execute(&ctx) {
            Ok(Value::Bool(b)) => Ok(b),
            Ok(other) => Err(ExprError::NotBool {
                got: value_variant_name(&other).to_string(),
            }),
            Err(e) => Err(ExprError::EvalFailed(e.to_string())),
        }
    }
}

/// Stable name for a `cel_interpreter::Value` variant. Avoids `Debug`
/// (which is not API-stable across cel-interpreter patch releases).
fn value_variant_name(v: &Value) -> &'static str {
    match v {
        Value::List(_) => "List",
        Value::Map(_) => "Map",
        Value::Function(_, _) => "Function",
        Value::Int(_) => "Int",
        Value::UInt(_) => "UInt",
        Value::Float(_) => "Float",
        Value::String(_) => "String",
        Value::Bytes(_) => "Bytes",
        Value::Bool(_) => "Bool",
        Value::Null => "Null",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_literal_true() {
        let engine = CelEngine::new();
        let expr = engine.compile("true").expect("compile");
        assert!(engine.eval_bool(&expr).expect("eval"));
    }

    #[test]
    fn evaluates_literal_false() {
        let engine = CelEngine::new();
        let expr = engine.compile("false").expect("compile");
        assert!(!engine.eval_bool(&expr).expect("eval"));
    }

    #[test]
    fn evaluates_integer_equality() {
        let engine = CelEngine::new();
        let expr = engine.compile("1 + 1 == 2").expect("compile");
        assert!(engine.eval_bool(&expr).expect("eval"));
    }

    #[test]
    fn evaluates_string_equality() {
        let engine = CelEngine::new();
        let expr = engine.compile("'a' == 'a'").expect("compile");
        assert!(engine.eval_bool(&expr).expect("eval"));
    }

    #[test]
    fn returns_not_bool_for_non_bool_expression() {
        let engine = CelEngine::new();
        let expr = engine.compile("1 + 1").expect("compile");
        match engine.eval_bool(&expr) {
            Err(ExprError::NotBool { got }) => assert_eq!(got, "Int"),
            other => panic!("expected NotBool, got {other:?}"),
        }
    }

    #[test]
    fn returns_parse_failed_for_malformed_expression() {
        let engine = CelEngine::new();
        match engine.compile("1 +") {
            Err(ExprError::ParseFailed(_)) => {}
            other => panic!("expected ParseFailed, got {other:?}"),
        }
    }
}

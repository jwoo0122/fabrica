//! CEL expression engine for SDUI runtime.
//!
//! Iteration 6 extends the Iteration-5 trait with engine-neutral
//! evaluation: callers build an [`EvalEnv`] of typed variables, the
//! engine returns an [`EvalValue`], and an [`ExprCache`] amortizes the
//! parser across re-resolutions.
//!
//! The `cel-interpreter` crate is still the only implementation, but
//! its types (`objects::Value`, `Context`) do not leak through the
//! trait surface — keeping the pivot path to JSON Logic or a custom
//! DSL open (CLAUDE.md §"Expression language: CEL").
//!
//! References (checked 2026-04-21):
//!   - `cel-interpreter::Program` docs: <https://docs.rs/cel-interpreter/0.10/cel_interpreter/struct.Program.html>
//!   - `cel-interpreter::Context`: <https://docs.rs/cel-interpreter/0.10/cel_interpreter/context/enum.Context.html>
//!   - `cel-interpreter::objects::Value`: <https://docs.rs/cel-interpreter/0.10/cel_interpreter/objects/enum.Value.html>
//!   - CEL langdef (bool strictness): <https://github.com/google/cel-spec/blob/master/doc/langdef.md>

use cel_interpreter::objects::{Key as CelKey, Map as CelMap, Value as CelValue};
use cel_interpreter::{Context, Program};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Engine-neutral value produced by [`ExpressionEngine::eval`].
///
/// Intentionally narrower than `cel_interpreter::objects::Value` — the
/// variants are just the ones scene-state and UI-state need to round-trip.
/// Do not widen without a compelling reason: the whole point is to keep
/// the trait detachable from any single engine.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalValue {
    /// Missing value / CEL null.
    Null,
    /// Boolean.
    Bool(bool),
    /// Signed 64-bit integer.
    Int(i64),
    /// Unsigned 64-bit integer.
    UInt(u64),
    /// IEEE-754 double.
    Float(f64),
    /// Owned UTF-8 string.
    String(String),
    /// Ordered list of further [`EvalValue`]s.
    List(Vec<EvalValue>),
    /// String-keyed map. `BTreeMap` for deterministic ordering in tests.
    Map(BTreeMap<String, EvalValue>),
}

impl EvalValue {
    /// Stable variant name (used in `NotBool` error messages and tests).
    pub fn variant_name(&self) -> &'static str {
        match self {
            EvalValue::Null => "Null",
            EvalValue::Bool(_) => "Bool",
            EvalValue::Int(_) => "Int",
            EvalValue::UInt(_) => "UInt",
            EvalValue::Float(_) => "Float",
            EvalValue::String(_) => "String",
            EvalValue::List(_) => "List",
            EvalValue::Map(_) => "Map",
        }
    }
}

/// Typed variable environment for a single expression evaluation.
///
/// Authors bind top-level variables by name; nested access (`ui.hovered`,
/// `state.count`) falls out of the map shape.
#[derive(Debug, Clone, Default)]
pub struct EvalEnv {
    vars: HashMap<String, EvalValue>,
}

impl EvalEnv {
    /// Build an empty env — the Iteration-5 behavior (literal-only).
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind (or overwrite) a top-level variable.
    pub fn set(&mut self, name: impl Into<String>, value: EvalValue) {
        self.vars.insert(name.into(), value);
    }

    /// Inspect a top-level variable (used in tests).
    pub fn get(&self, name: &str) -> Option<&EvalValue> {
        self.vars.get(name)
    }
}

/// LRU-free cache of compiled expressions, keyed by source text.
///
/// Owned by the app (native / web) and reused across every
/// re-resolution, so `Program::compile` — which is the expensive part of
/// `cel-interpreter` — runs at most once per unique expression per
/// session. Codex review finding #7: without this cache, per-hover
/// compile cost is a self-inflicted DoS.
#[derive(Debug, Default)]
pub struct ExprCache {
    entries: HashMap<String, CompiledExpr>,
}

impl ExprCache {
    /// Construct an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a compiled expression for `src`, compiling on first sight.
    ///
    /// The returned reference lives as long as `&mut self` — callers
    /// evaluate immediately and drop the borrow before mutating the
    /// cache again.
    pub fn compile_cached<E: ExpressionEngine>(
        &mut self,
        engine: &E,
        src: &str,
    ) -> Result<&CompiledExpr, ExprError> {
        use std::collections::hash_map::Entry;
        match self.entries.entry(src.to_string()) {
            Entry::Occupied(e) => Ok(e.into_mut()),
            Entry::Vacant(v) => {
                let compiled = engine.compile(src)?;
                Ok(v.insert(compiled))
            }
        }
    }

    /// Number of cached entries (tests).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true when no entries are cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Smallest viable surface for a runtime expression engine.
///
/// Implementors provide `compile` + `eval`. `eval_bool` is derived
/// from `eval` with a standard strictness check.
pub trait ExpressionEngine {
    /// Parse `src` into a compiled, reusable expression.
    fn compile(&self, src: &str) -> Result<CompiledExpr, ExprError>;

    /// Evaluate `expr` against `env`, returning the typed result.
    fn eval(&self, expr: &CompiledExpr, env: &EvalEnv) -> Result<EvalValue, ExprError>;

    /// Evaluate `expr` against `env` and require a boolean result.
    ///
    /// Default implementation rejects non-bool results with
    /// [`ExprError::NotBool`] — mirrors the Iteration-5 strictness.
    fn eval_bool(&self, expr: &CompiledExpr, env: &EvalEnv) -> Result<bool, ExprError> {
        match self.eval(expr, env)? {
            EvalValue::Bool(b) => Ok(b),
            other => Err(ExprError::NotBool {
                got: other.variant_name().to_string(),
            }),
        }
    }
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
        /// Stable variant name of the non-bool result (e.g. `"Int"`).
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

    fn eval(&self, expr: &CompiledExpr, env: &EvalEnv) -> Result<EvalValue, ExprError> {
        let ctx = build_context(env);
        match expr.0.execute(&ctx) {
            Ok(value) => Ok(cel_to_eval(&value)),
            Err(e) => Err(ExprError::EvalFailed(e.to_string())),
        }
    }
}

// ── cel-interpreter <-> EvalValue bridge ────────────────────────────────

fn build_context(env: &EvalEnv) -> Context<'static> {
    let mut ctx = Context::default();
    for (name, value) in &env.vars {
        ctx.add_variable_from_value(name.clone(), eval_to_cel(value));
    }
    ctx
}

fn eval_to_cel(v: &EvalValue) -> CelValue {
    match v {
        EvalValue::Null => CelValue::Null,
        EvalValue::Bool(b) => CelValue::Bool(*b),
        EvalValue::Int(i) => CelValue::Int(*i),
        EvalValue::UInt(u) => CelValue::UInt(*u),
        EvalValue::Float(f) => CelValue::Float(*f),
        EvalValue::String(s) => CelValue::String(Arc::new(s.clone())),
        EvalValue::List(items) => {
            let out: Vec<CelValue> = items.iter().map(eval_to_cel).collect();
            CelValue::List(Arc::new(out))
        }
        EvalValue::Map(m) => {
            let mut out: HashMap<CelKey, CelValue> = HashMap::with_capacity(m.len());
            for (k, v) in m {
                out.insert(CelKey::String(Arc::new(k.clone())), eval_to_cel(v));
            }
            CelValue::Map(CelMap {
                map: Arc::new(out),
            })
        }
    }
}

fn cel_to_eval(v: &CelValue) -> EvalValue {
    match v {
        CelValue::Null => EvalValue::Null,
        CelValue::Bool(b) => EvalValue::Bool(*b),
        CelValue::Int(i) => EvalValue::Int(*i),
        CelValue::UInt(u) => EvalValue::UInt(*u),
        CelValue::Float(f) => EvalValue::Float(*f),
        CelValue::String(s) => EvalValue::String(s.as_str().to_owned()),
        CelValue::Bytes(_) => EvalValue::Null, // Bytes has no scene use — collapse to Null.
        CelValue::List(items) => EvalValue::List(items.iter().map(cel_to_eval).collect()),
        CelValue::Map(m) => {
            let mut out = BTreeMap::new();
            for (k, v) in m.map.iter() {
                if let CelKey::String(s) = k {
                    out.insert(s.as_str().to_owned(), cel_to_eval(v));
                }
                // Non-string keys are not used by scene state — drop silently.
            }
            EvalValue::Map(out)
        }
        CelValue::Function(_, _) => EvalValue::Null,
        #[allow(unreachable_patterns)]
        _ => EvalValue::Null, // Duration/Timestamp feature-gated — fall through.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_literal_true() {
        let engine = CelEngine::new();
        let expr = engine.compile("true").expect("compile");
        let env = EvalEnv::new();
        assert!(engine.eval_bool(&expr, &env).expect("eval"));
    }

    #[test]
    fn evaluates_literal_false() {
        let engine = CelEngine::new();
        let expr = engine.compile("false").expect("compile");
        let env = EvalEnv::new();
        assert!(!engine.eval_bool(&expr, &env).expect("eval"));
    }

    #[test]
    fn evaluates_integer_equality() {
        let engine = CelEngine::new();
        let expr = engine.compile("1 + 1 == 2").expect("compile");
        let env = EvalEnv::new();
        assert!(engine.eval_bool(&expr, &env).expect("eval"));
    }

    #[test]
    fn evaluates_string_equality() {
        let engine = CelEngine::new();
        let expr = engine.compile("'a' == 'a'").expect("compile");
        let env = EvalEnv::new();
        assert!(engine.eval_bool(&expr, &env).expect("eval"));
    }

    #[test]
    fn returns_not_bool_for_non_bool_expression() {
        let engine = CelEngine::new();
        let expr = engine.compile("1 + 1").expect("compile");
        let env = EvalEnv::new();
        match engine.eval_bool(&expr, &env) {
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

    #[test]
    fn eval_returns_typed_int() {
        let engine = CelEngine::new();
        let expr = engine.compile("1 + 2").expect("compile");
        let env = EvalEnv::new();
        assert_eq!(engine.eval(&expr, &env).expect("eval"), EvalValue::Int(3));
    }

    #[test]
    fn eval_reads_top_level_variable() {
        let engine = CelEngine::new();
        let expr = engine.compile("x + 1").expect("compile");
        let mut env = EvalEnv::new();
        env.set("x", EvalValue::Int(41));
        assert_eq!(engine.eval(&expr, &env).expect("eval"), EvalValue::Int(42));
    }

    #[test]
    fn eval_reads_nested_map_variable() {
        let engine = CelEngine::new();
        let expr = engine.compile("state.count + 1").expect("compile");
        let mut env = EvalEnv::new();
        let mut state = BTreeMap::new();
        state.insert("count".to_string(), EvalValue::Int(7));
        env.set("state", EvalValue::Map(state));
        assert_eq!(engine.eval(&expr, &env).expect("eval"), EvalValue::Int(8));
    }

    #[test]
    fn eval_bool_compares_string_against_null() {
        // Codex finding #1: `null == "x"` must be false, not error.
        let engine = CelEngine::new();
        let expr = engine
            .compile("ui.hovered == 'counter_btn'")
            .expect("compile");
        let mut env = EvalEnv::new();
        let mut ui = BTreeMap::new();
        ui.insert("hovered".to_string(), EvalValue::Null);
        ui.insert("pressed".to_string(), EvalValue::Null);
        env.set("ui", EvalValue::Map(ui));
        assert!(!engine.eval_bool(&expr, &env).expect("eval"));
    }

    #[test]
    fn eval_bool_matches_string_in_map() {
        let engine = CelEngine::new();
        let expr = engine
            .compile("ui.hovered == 'counter_btn'")
            .expect("compile");
        let mut env = EvalEnv::new();
        let mut ui = BTreeMap::new();
        ui.insert(
            "hovered".to_string(),
            EvalValue::String("counter_btn".into()),
        );
        ui.insert("pressed".to_string(), EvalValue::Null);
        env.set("ui", EvalValue::Map(ui));
        assert!(engine.eval_bool(&expr, &env).expect("eval"));
    }

    #[test]
    fn eval_missing_top_level_variable_errors() {
        // Codex finding #1: undeclared variables raise EvalFailed;
        // callers must pre-declare everything they reference.
        let engine = CelEngine::new();
        let expr = engine.compile("no_such_var").expect("compile");
        let env = EvalEnv::new();
        match engine.eval(&expr, &env) {
            Err(ExprError::EvalFailed(_)) => {}
            other => panic!("expected EvalFailed, got {other:?}"),
        }
    }

    #[test]
    fn eval_value_variant_names() {
        assert_eq!(EvalValue::Null.variant_name(), "Null");
        assert_eq!(EvalValue::Bool(true).variant_name(), "Bool");
        assert_eq!(EvalValue::Int(0).variant_name(), "Int");
        assert_eq!(EvalValue::UInt(0).variant_name(), "UInt");
        assert_eq!(EvalValue::Float(0.0).variant_name(), "Float");
        assert_eq!(EvalValue::String("".into()).variant_name(), "String");
        assert_eq!(EvalValue::List(vec![]).variant_name(), "List");
        assert_eq!(EvalValue::Map(BTreeMap::new()).variant_name(), "Map");
    }

    #[test]
    fn expr_cache_returns_same_entry_for_same_source() {
        let engine = CelEngine::new();
        let mut cache = ExprCache::new();
        let _ = cache.compile_cached(&engine, "true").expect("compile1");
        assert_eq!(cache.len(), 1);
        let _ = cache.compile_cached(&engine, "true").expect("compile2");
        assert_eq!(cache.len(), 1, "same source should not add an entry");
        let _ = cache.compile_cached(&engine, "false").expect("compile3");
        assert_eq!(cache.len(), 2, "new source adds an entry");
    }

    #[test]
    fn expr_cache_propagates_parse_errors_and_does_not_cache_them() {
        let engine = CelEngine::new();
        let mut cache = ExprCache::new();
        match cache.compile_cached(&engine, "1 +") {
            Err(ExprError::ParseFailed(_)) => {}
            other => panic!("expected ParseFailed, got {other:?}"),
        }
        assert_eq!(
            cache.len(),
            0,
            "failed compiles must not poison the cache"
        );
    }
}

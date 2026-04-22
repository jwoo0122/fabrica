//! `examples/mock-server`: a static, no-auth JSON + asset server used by
//! future Fetching-node examples. See CLAUDE.md §"Fetching node".
//!
//! Endpoints:
//!   GET /health       → 200 "ok"
//!   GET /api/items    → 200 application/json `{"items":[]}`
//!   GET /assets/*path → static files from `<workspace-root>/assets/images/`
//!                       (Content-Type inferred by tower-http ServeDir)
//!
//! Port is `MOCK_PORT` env var or first CLI arg; defaults to 8138.

use axum::{routing::get, Json, Router};
use serde_json::json;
use std::net::SocketAddr;
use std::path::PathBuf;
use tower_http::{cors::CorsLayer, services::ServeDir};

async fn health() -> &'static str {
    "ok"
}

async fn items() -> Json<serde_json::Value> {
    Json(json!({ "items": [] }))
}

fn resolve_port() -> u16 {
    if let Some(arg) = std::env::args().nth(1) {
        if let Ok(p) = arg.parse::<u16>() {
            return p;
        }
    }
    if let Ok(p) = std::env::var("MOCK_PORT") {
        if let Ok(n) = p.parse::<u16>() {
            return n;
        }
    }
    8138
}

/// Resolve `<workspace-root>/assets/images/` via this crate's manifest dir.
///
/// The `/assets/*` URL path maps to this directory, so e.g.
/// `GET /assets/smoke.png` serves `<root>/assets/images/smoke.png`.
///
/// `env!("CARGO_MANIFEST_DIR")` is baked in at compile time. If the workspace
/// is moved or renamed on disk, a stale cached binary would silently point at
/// a non-existent path — the `exists()` check below turns that into a loud
/// startup failure with an explicit rebuild hint.
fn assets_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = examples/mock-server; ../../assets reaches root.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("mock-server lives under examples/")
        .join("assets")
        .join("images");
    if !path.exists() {
        eprintln!(
            "mock-server: assets dir does not exist: {}\n\
             The binary was likely compiled against an old workspace location.\n\
             Fix: cargo clean -p mock-server && cargo build -p mock-server",
            path.display()
        );
        std::process::exit(1);
    }
    path
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = resolve_port();
    let assets = assets_dir();

    // Permissive CORS — the examples run app-web on :8137 and the mock-server
    // on :8138, which browsers treat as cross-origin. Without this header,
    // wasm `fetch(..., mode: "cors")` calls are blocked client-side.
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/items", get(items))
        .nest_service("/assets", ServeDir::new(&assets))
        .layer(CorsLayer::permissive());

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!(
        "mock-server listening on http://{addr} (assets from {})",
        assets.display()
    );
    axum::serve(listener, app).await?;
    Ok(())
}

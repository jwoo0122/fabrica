//! Iteration 0 `examples/mock-server`: a static, no-auth JSON server used by
//! future Fetching-node examples. See CLAUDE.md §"Fetching node".
//!
//! Endpoints in this iteration:
//!   GET /health  → 200 "ok"
//!   GET /api/items → 200 application/json `{"items":[]}`
//!
//! Port is `MOCK_PORT` env var or first CLI arg; defaults to 8138.

use axum::{routing::get, Json, Router};
use serde_json::json;
use std::net::SocketAddr;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = resolve_port();
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/items", get(items));

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("mock-server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

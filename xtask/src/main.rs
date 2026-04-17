//! `cargo xtask` — workspace automation. See `.iteration-0-criteria.md` §3.
//!
//! Subcommands:
//!   * build-all        — `cargo build` for native + wasm (auto-adds rustup target).
//!   * run-native       — build + launch `app-native` (foreground or detached).
//!   * run-web          — wasm build + static HTTP server for the app-web bundle.
//!   * inspect-ax       — read AccessKit tree dump from a running native app.
//!
//! References checked on 2026-04-17:
//!   * wasm-bindgen CLI: <https://rustwasm.github.io/wasm-bindgen/reference/cli.html>
//!   * axum 0.8 routing: <https://docs.rs/axum/0.8/axum/>
//!   * tower-http `ServeDir`: <https://docs.rs/tower-http/0.6/tower_http/services/struct.ServeDir.html>

use clap::{Parser, Subcommand};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

#[derive(Parser)]
#[command(name = "xtask", about = "rust-sdui-test workspace automation")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build native app + wasm app.
    BuildAll,
    /// Launch `app-native`.
    RunNative {
        #[arg(long, default_value = "smoke")]
        example: String,
        #[arg(long)]
        background: bool,
        #[arg(long)]
        pidfile: Option<PathBuf>,
    },
    /// Build wasm + serve it over HTTP.
    RunWeb {
        #[arg(long, default_value = "smoke")]
        example: String,
        #[arg(long)]
        background: bool,
        #[arg(long, default_value_t = 8137)]
        port: u16,
        #[arg(long)]
        pidfile: Option<PathBuf>,
    },
    /// Dump AccessKit tree for a running native process.
    InspectAx {
        #[arg(long)]
        pid: i64,
    },
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at xtask/; parent is the workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is nested under workspace root")
        .to_path_buf()
}

fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

fn run(mut cmd: Command, context: &str) -> std::io::Result<()> {
    let status = cmd.status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!("{context} failed: {status}")));
    }
    Ok(())
}

fn ensure_wasm_target() -> std::io::Result<()> {
    // Probe via `rustup target list --installed` — cheap and scriptable.
    let out = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.lines().any(|l| l.trim() == "wasm32-unknown-unknown") {
        return Ok(());
    }
    eprintln!("[xtask] installing wasm32-unknown-unknown target via rustup");
    run(
        {
            let mut c = Command::new("rustup");
            c.args(["target", "add", "wasm32-unknown-unknown"]);
            c
        },
        "rustup target add",
    )
}

fn ensure_wasm_bindgen_cli() -> std::io::Result<()> {
    if Command::new("wasm-bindgen")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return Ok(());
    }
    eprintln!("[xtask] installing wasm-bindgen-cli (required for app-web glue)");
    run(
        {
            let mut c = Command::new("cargo");
            c.args(["install", "wasm-bindgen-cli"]);
            c
        },
        "cargo install wasm-bindgen-cli",
    )
}

/// Fixed port for the bundled mock-server. Example scene.json files reference
/// `http://127.0.0.1:8138/assets/...` directly.
const MOCK_SERVER_PORT: u16 = 8138;

/// Build mock-server, spawn it as a child on `MOCK_SERVER_PORT`, print its PID,
/// and return the child handle. Callers are expected to kill it when the main
/// app exits.
fn spawn_mock_server(root: &Path) -> std::io::Result<Child> {
    // Build up front so the child's stdout isn't polluted with compile logs.
    run(
        {
            let mut c = cargo();
            c.current_dir(root).args(["build", "-p", "mock-server"]);
            c
        },
        "cargo build -p mock-server",
    )?;

    let bin = root.join("target").join("debug").join("mock-server");
    if !bin.exists() {
        return Err(std::io::Error::other(format!(
            "mock-server binary missing: {}",
            bin.display()
        )));
    }

    let child = Command::new(&bin)
        .arg(MOCK_SERVER_PORT.to_string())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    eprintln!(
        "[xtask] mock-server pid={} on http://127.0.0.1:{MOCK_SERVER_PORT}",
        child.id()
    );
    Ok(child)
}

/// Best-effort `kill` for a background child. Errors are logged, never propagated.
fn stop_mock_server(child: &mut Child) {
    if let Err(e) = child.kill() {
        eprintln!("[xtask] warning: failed to kill mock-server: {e}");
    }
    let _ = child.wait();
}

fn cmd_build_all() -> std::io::Result<()> {
    let root = workspace_root();
    ensure_wasm_target()?;
    run(
        {
            let mut c = cargo();
            c.current_dir(&root).args(["build", "-p", "app-native"]);
            c
        },
        "cargo build -p app-native",
    )?;
    run(
        {
            let mut c = cargo();
            c.current_dir(&root).args([
                "build",
                "-p",
                "app-web",
                "--target",
                "wasm32-unknown-unknown",
            ]);
            c
        },
        "cargo build -p app-web --target wasm32-unknown-unknown",
    )
}

fn cmd_run_native(example: &str, background: bool, pidfile: Option<&Path>) -> std::io::Result<()> {
    let root = workspace_root();
    // Build first so `run` doesn't interleave compile output with window init.
    run(
        {
            let mut c = cargo();
            c.current_dir(&root).args(["build", "-p", "app-native"]);
            c
        },
        "cargo build -p app-native",
    )?;

    let bin = root.join("target").join("debug").join("app-native");
    if !bin.exists() {
        return Err(std::io::Error::other(format!(
            "native binary missing: {}",
            bin.display()
        )));
    }

    // Co-boot mock-server so the scene can fetch its bundled assets on first paint.
    let mut mock_child = spawn_mock_server(&root)?;

    let mut cmd = Command::new(&bin);
    cmd.arg("--example").arg(example);

    if background {
        let child = cmd.spawn()?;
        if let Some(p) = pidfile {
            std::fs::write(p, child.id().to_string())?;
        }
        // Detach the main app; mock-server stays a zombie-free background
        // process whose lifetime is independent. /verify-* scripts are
        // expected to clean it up via the printed PID.
        eprintln!(
            "[xtask] background mode: app-native pid written to pidfile; \
             mock-server pid={} must be stopped separately",
            mock_child.id()
        );
        // Forget the Child so its destructor doesn't try to kill on drop.
        std::mem::forget(mock_child);
        return Ok(());
    }

    // Foreground: block on app; tear down mock-server when it exits.
    let status_result = cmd.status();
    stop_mock_server(&mut mock_child);
    let status = status_result?;
    if !status.success() {
        return Err(std::io::Error::other(format!("app-native exit {status}")));
    }
    Ok(())
}

/// Build the wasm artefact + JS glue into `target/app-web-dist/`.
fn build_web_dist(root: &Path) -> std::io::Result<PathBuf> {
    ensure_wasm_target()?;
    ensure_wasm_bindgen_cli()?;

    run(
        {
            let mut c = cargo();
            c.current_dir(root).args([
                "build",
                "-p",
                "app-web",
                "--target",
                "wasm32-unknown-unknown",
            ]);
            c
        },
        "cargo build -p app-web --target wasm32-unknown-unknown",
    )?;

    let dist = root.join("target").join("app-web-dist");
    std::fs::create_dir_all(&dist)?;

    let wasm_in = root
        .join("target")
        .join("wasm32-unknown-unknown")
        .join("debug")
        .join("app_web.wasm");
    if !wasm_in.exists() {
        return Err(std::io::Error::other(format!(
            "wasm artefact missing: {}",
            wasm_in.display()
        )));
    }

    // wasm-bindgen CLI: emits app_web.js + app_web_bg.wasm in --out-dir.
    run(
        {
            let mut c = Command::new("wasm-bindgen");
            c.arg("--target")
                .arg("web")
                .arg("--no-typescript")
                .arg("--out-dir")
                .arg(&dist)
                .arg(&wasm_in);
            c
        },
        "wasm-bindgen",
    )?;

    // Copy the loader HTML beside the wasm bundle.
    let html_src = root.join("app-web").join("static").join("index.html");
    let html_dst = dist.join("index.html");
    std::fs::copy(&html_src, &html_dst)?;

    Ok(dist)
}

fn cmd_run_web(
    example: &str,
    background: bool,
    port: u16,
    pidfile: Option<&Path>,
) -> std::io::Result<()> {
    let root = workspace_root();
    let dist = build_web_dist(&root)?;

    // Co-boot mock-server so fetches from the wasm app succeed first-try.
    let mut mock_child = spawn_mock_server(&root)?;

    if background {
        // Re-exec ourselves with a hidden subcommand so the child actually
        // supervises the tokio runtime; the parent writes the pidfile + exits.
        let current_exe = std::env::current_exe()?;
        let mut cmd = Command::new(current_exe);
        cmd.arg("__serve-web-foreground")
            .arg("--dist")
            .arg(&dist)
            .arg("--port")
            .arg(port.to_string())
            .arg("--example")
            .arg(example);
        let child = cmd.spawn()?;
        if let Some(p) = pidfile {
            std::fs::write(p, child.id().to_string())?;
        }
        eprintln!(
            "[xtask] background mode: app-web pid written to pidfile; \
             mock-server pid={} must be stopped separately",
            mock_child.id()
        );
        std::mem::forget(mock_child);
        return Ok(());
    }

    let result = serve_web_blocking(&dist, port, example);
    stop_mock_server(&mut mock_child);
    result
}

fn serve_web_blocking(dist: &Path, port: u16, _example: &str) -> std::io::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        use axum::{response::IntoResponse, routing::get, Router};
        use tower_http::services::ServeDir;

        let dist = dist.to_path_buf();
        // `/examples/<name>` and `/examples/<name>/` both serve the wasm
        // bundle — the example name is a placeholder in Iteration 0.
        let serve_bundle = ServeDir::new(dist.clone()).append_index_html_on_directories(true);
        let serve_bundle_2 = ServeDir::new(dist).append_index_html_on_directories(true);

        let app = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route(
                "/",
                get(|| async { axum::response::Redirect::to("/examples/smoke/") }),
            )
            .nest_service("/examples/smoke", serve_bundle)
            // Fallback route for any future example name; points at the same
            // bundle until per-example builds exist.
            .nest_service("/examples", serve_bundle_2)
            .fallback(|| async {
                (axum::http::StatusCode::NOT_FOUND, "not found").into_response()
            });

        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        eprintln!("[xtask] serving app-web on http://{addr}");
        axum::serve(listener, app).await?;
        Ok::<_, std::io::Error>(())
    })
}

fn cmd_inspect_ax(pid: i64) -> std::io::Result<()> {
    // Iteration 1: read the AccessKit tree dump that app-native writes to
    // `target/ax-dump-<pid>.json` on startup. This avoids NSAccessibility
    // crawling while still providing real Frame node data.
    let root = workspace_root();
    let dump_path = root.join("target").join(format!("ax-dump-{pid}.json"));

    if dump_path.exists() {
        let contents = std::fs::read_to_string(&dump_path)?;
        // Validate it's proper JSON, then print.
        let doc: serde_json::Value =
            serde_json::from_str(&contents).map_err(std::io::Error::other)?;
        println!("{}", serde_json::to_string_pretty(&doc).unwrap());
    } else {
        // Fallback: if the dump file doesn't exist yet, wait briefly and retry.
        eprintln!(
            "[inspect-ax] dump file not found at {}, retrying in 2s...",
            dump_path.display()
        );
        std::thread::sleep(std::time::Duration::from_secs(2));
        if dump_path.exists() {
            let contents = std::fs::read_to_string(&dump_path)?;
            let doc: serde_json::Value =
                serde_json::from_str(&contents).map_err(std::io::Error::other)?;
            println!("{}", serde_json::to_string_pretty(&doc).unwrap());
        } else {
            return Err(std::io::Error::other(format!(
                "ax-dump file not found for pid {pid} at {}",
                dump_path.display()
            )));
        }
    }
    Ok(())
}

fn hidden_serve_web_foreground(args: &[String]) -> std::io::Result<()> {
    // Minimal hand-rolled parse: called only by our own background spawn.
    let mut dist: Option<PathBuf> = None;
    let mut port: Option<u16> = None;
    let mut example = String::from("smoke");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dist" => {
                dist = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--port" => {
                port = Some(args[i + 1].parse().map_err(std::io::Error::other)?);
                i += 2;
            }
            "--example" => {
                example = args[i + 1].clone();
                i += 2;
            }
            other => {
                return Err(std::io::Error::other(format!(
                    "unexpected arg to __serve-web-foreground: {other}"
                )));
            }
        }
    }
    let dist = dist.ok_or_else(|| std::io::Error::other("missing --dist"))?;
    let port = port.ok_or_else(|| std::io::Error::other("missing --port"))?;
    serve_web_blocking(&dist, port, &example)
}

fn main() -> std::io::Result<()> {
    // Intercept the hidden re-exec subcommand before clap sees it.
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.get(1).map(String::as_str) == Some("__serve-web-foreground") {
        return hidden_serve_web_foreground(&raw_args[2..]);
    }

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::BuildAll => cmd_build_all(),
        Cmd::RunNative {
            example,
            background,
            pidfile,
        } => cmd_run_native(&example, background, pidfile.as_deref()),
        Cmd::RunWeb {
            example,
            background,
            port,
            pidfile,
        } => cmd_run_web(&example, background, port, pidfile.as_deref()),
        Cmd::InspectAx { pid } => cmd_inspect_ax(pid),
    }
}

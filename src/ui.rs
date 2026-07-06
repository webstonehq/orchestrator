//! Embedded web UI serving (single-file SvelteKit build).
//!
//! The SvelteKit app under `ui/` builds to one self-contained file,
//! `ui/build/index.html` (adapter-static + `bundleStrategy: 'inline'` +
//! pathname-routed SPA — all JS/CSS/fonts inlined, no external asset
//! references).
//!
//! - **Debug builds** read `ui/build/index.html` from disk on every request
//!   (relative to `CARGO_MANIFEST_DIR`), so the UI can be rebuilt without
//!   recompiling Rust. If the file is missing, a fallback page with build
//!   instructions is served instead.
//! - **Release builds** embed the file at compile time via [`include_str!`].
//!   This means `ui/build/index.html` **must exist** when compiling in
//!   release mode (`cd ui && npm install && npm run build` first) or
//!   compilation fails — intentional, so a release binary can never ship
//!   without the UI.

use axum::Router;
use axum::http::{StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{any, get};
use serde_json::json;

/// Served in debug builds when `ui/build/index.html` does not exist yet.
#[cfg(debug_assertions)]
const NOT_BUILT: &str = "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>Orchestrator</title></head>\n<body style=\"background:#0a0c10;color:#e7edf5;font-family:system-ui,sans-serif;display:grid;place-items:center;min-height:100vh;margin:0\">\n<div style=\"text-align:center\"><h1>UI not built</h1><p>Run: <code style=\"color:#7ee787\">cd ui &amp;&amp; npm install &amp;&amp; npm run build</code></p></div>\n</body></html>\n";

/// Router serving the embedded UI: `/` plus a fallback for every other
/// unmatched path and method. Unmatched `/api/*` paths get a JSON 404 (any
/// method) so typo'd API endpoints never masquerade as successes; all other
/// unmatched paths get the SPA HTML — required under pathname routing, since
/// deep links like `/runs/42` arrive as real server requests and must load
/// the app for the client router to take over.
/// Registered routes (e.g. `/api/health`) keep their own method semantics.
pub fn router() -> Router {
    Router::new()
        .route("/", get(serve_index))
        .fallback(any(fallback))
}

async fn serve_index() -> Response {
    Html(index_html()).into_response()
}

async fn fallback(uri: Uri) -> Response {
    if uri.path().starts_with("/api/") {
        (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": "not found"})),
        )
            .into_response()
    } else {
        serve_index().await
    }
}

/// Debug: re-read the built UI from disk on every request so UI rebuilds
/// show up without restarting/recompiling the server.
#[cfg(debug_assertions)]
fn index_html() -> std::borrow::Cow<'static, str> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/ui/build/index.html");
    match std::fs::read_to_string(path) {
        Ok(html) => html.into(),
        Err(err) => {
            tracing::warn!(path, %err, "ui/build/index.html not readable; serving fallback page");
            NOT_BUILT.into()
        }
    }
}

/// Release: the UI is embedded into the binary at compile time.
#[cfg(not(debug_assertions))]
fn index_html() -> std::borrow::Cow<'static, str> {
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/ui/build/index.html")).into()
}

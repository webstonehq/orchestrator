//! Test-support helpers for standing up the `http` plugin bundle in tests.
//!
//! Not part of the public API — plugins are ordinarily discovered from a
//! plugins directory. Tests need the shipped `http` bundle available: some just
//! want its manifest (fast, no binary), others execute it (the real built
//! binary, staged into a temp bundle).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use super::{Executor, PluginEntry, PluginManifest, PluginRegistry};

/// The committed http bundle descriptor (manifest + lifecycle + metadata).
const HTTP_PLUGIN_JSON: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/plugins/http/plugin.json"));

/// Build `plugin-http` once, into an isolated target dir (so the nested build
/// never contends on the outer `cargo test` lock), and return its binary path.
fn http_binary() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let target = std::env::temp_dir().join("orch-plugin-http-target");
        let status = std::process::Command::new(env!("CARGO"))
            .args(["build", "-p", "plugin-http"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("CARGO_TARGET_DIR", &target)
            .status()
            .expect("run cargo build -p plugin-http");
        assert!(status.success(), "cargo build -p plugin-http failed");
        let bin = target.join("debug").join("orchestrator-plugin-http");
        assert!(bin.exists(), "plugin-http binary missing at {}", bin.display());
        bin
    })
}

/// Stage the http bundle (plugin.json + built binary) once, returning the
/// plugins-dir that contains the `http/` bundle. Pass to `load_external`.
pub fn staged_http_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let root = std::env::temp_dir().join("orch-http-bundle");
        let bundle = root.join("http");
        std::fs::create_dir_all(&bundle).expect("create bundle dir");
        std::fs::write(bundle.join("plugin.json"), HTTP_PLUGIN_JSON).expect("write plugin.json");
        std::fs::copy(http_binary(), bundle.join("orchestrator-plugin-http")).expect("copy binary");
        root
    })
}

/// A registry with the real http plugin loaded (executes the built binary).
pub fn http_registry() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry.load_external(staged_http_dir());
    registry
}

/// A registry with the http *manifest* only — no binary, no execution. For unit
/// tests that need the schema/manifest but never run a task.
pub fn manifest_registry() -> PluginRegistry {
    let bundle: serde_json::Value = serde_json::from_str(HTTP_PLUGIN_JSON).unwrap();
    let manifest: PluginManifest = serde_json::from_value(bundle["manifest"].clone()).unwrap();
    let mut registry = PluginRegistry::new();
    registry.insert(PluginEntry {
        manifest,
        version: Some("0.1.0".to_string()),
        supports_validate: false,
        program: PathBuf::from("orchestrator-plugin-http"),
        args: Vec::new(),
        cwd: PathBuf::from("."),
        term_grace: Duration::from_secs(3),
        validate_timeout: Duration::from_secs(3),
        executor: Executor::Oneshot,
    });
    registry
}

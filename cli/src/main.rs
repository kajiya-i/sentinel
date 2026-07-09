//! `sentinel` — the Sentinel CLI. Composes the concrete adapters
//! (`sentinel-browser`, `sentinel-ai`, `sentinel-store`) into the `sentinel-core`
//! orchestration and exposes `check` / `run` / `eval` subcommands.
//!
//! Skeleton: command wiring lands in M5 (T-M5-01); for now `main` only
//! installs logging so events emitted as the crates grow are captured.

mod logging;

fn main() {
    logging::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "sentinel starting");
}

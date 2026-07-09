//! Tracing subscriber initialization for the CLI.
//!
//! The `cli` owns logging; library crates (`core`/`browser`/`ai`/`store`) only emit
//! events and never install a subscriber (docs/rules/logging.md). Diagnostics go to
//! **stderr** so **stdout stays clean for machine-readable reports**.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// Level filter used when `RUST_LOG` is unset, empty, or malformed.
const DEFAULT_FILTER: &str = "info";

/// Resolve the filter directive from an optional `RUST_LOG` value.
///
/// Falls back to [`DEFAULT_FILTER`] on absent/blank input; a non-blank value is
/// passed through and validated by the caller.
fn resolve_filter(raw: Option<&str>) -> &str {
    match raw {
        Some(s) if !s.trim().is_empty() => s,
        _ => DEFAULT_FILTER,
    }
}

/// Build the [`EnvFilter`] from `RUST_LOG`, falling back to [`DEFAULT_FILTER`]
/// when the variable is unset, blank, or an invalid directive — a bad env var
/// must not abort startup.
fn env_filter() -> EnvFilter {
    let raw = std::env::var("RUST_LOG").ok();
    EnvFilter::try_new(resolve_filter(raw.as_deref()))
        // INVARIANT: DEFAULT_FILTER is a valid single-level directive.
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
}

/// Install the process-wide tracing subscriber. Call once, early in `main`.
///
/// Level is controlled by `RUST_LOG` (default `info`). ANSI colors are enabled
/// only when stderr is a terminal, so redirected logs carry no escape codes.
pub fn init() {
    tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_filter_should_default_to_info_when_unset() {
        assert_eq!(resolve_filter(None), "info");
        assert_eq!(resolve_filter(Some("   ")), "info");
    }

    #[test]
    fn resolve_filter_should_use_rust_log_when_set() {
        assert_eq!(resolve_filter(Some("debug")), "debug");
        assert_eq!(
            resolve_filter(Some("sentinel_cli=trace")),
            "sentinel_cli=trace"
        );
    }
}

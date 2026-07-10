//! Typed error skeletons for Sentinel (thiserror).
//!
//! All port error types live in `core` and are **adapter-agnostic**: they carry no
//! reqwest / chromiumoxide / sqlx types (docs/rules/design.md — never leak adapter types
//! into `core`). Adapters map their library errors into these variants (e.g. a reqwest
//! error becomes [`AiError::Transport`]). This is deliberate: a `RunError` that did
//! `#[from] browser::BrowserError` would make `core` depend on `browser`, and since
//! `browser` already depends on `core`, that is a dependency cycle. Keeping every error
//! in `core` lets [`RunError`] aggregate them with `#[from]` and keeps orchestration
//! (which lives in `core`) able to name them.
//!
//! Error messages must not embed secrets (API keys) or raw spec/DOM (docs/rules/logging.md).

/// Config load / validation failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("yaml parse error: {0}")]
    Parse(String),
    #[error("empty spec in check {name}")]
    EmptySpec { name: String },
    #[error("threshold out of range (0.0..=1.0): {0}")]
    ThresholdRange(f32),
    #[error("invalid target selector: {0}")]
    InvalidTarget(String),
    #[error("invalid or unresolvable url: {url}")]
    InvalidUrl { url: String },
    #[error("invalid check {name}: {reason}")]
    Invalid { name: String, reason: String },
}

/// Browser (CDP) failures. Adapter-agnostic: chromiumoxide errors are mapped into
/// [`BrowserError::Protocol`] by the `browser` adapter.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BrowserError {
    #[error("chrome/chromium not found (set --chrome-path)")]
    ChromeNotFound,
    #[error("navigation to {url} failed")]
    Navigation { url: String },
    #[error("timed out after {ms}ms waiting for {target}")]
    Timeout { target: String, ms: u64 },
    #[error("element not found: {target}")]
    ElementNotFound { target: String },
    #[error("cdp protocol error: {0}")]
    Protocol(String),
}

/// AI (Judge) failures. Adapter-agnostic: reqwest transport errors are mapped into
/// [`AiError::Transport`] by the `ai` adapter — a `#[from] reqwest::Error` would leak
/// reqwest into `core`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AiError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("rate limited; retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("model refused the request (category: {category})")]
    Refusal { category: String },
    #[error("response did not match json_schema: {0}")]
    SchemaViolation(String),
}

/// Persistence failures (Phase 2). Adapter-agnostic: sqlx errors map into
/// [`StoreError::Backend`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("storage backend error: {0}")]
    Backend(String),
    #[error("{entity} not found")]
    NotFound { entity: String },
}

/// Top-level run failure, aggregating the per-area errors via `#[from]`.
///
/// A recoverable failure *while executing one check* is not a `RunError` — it becomes
/// `CheckResult { verdict: Error, .. }` so one check can't abort the run
/// (docs/rules/error-handling.md). `RunError` is for run-level failures (config load, I/O).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    #[error("config error: {0}")]
    Config(#[from] ConfigError),
    #[error("browser error: {0}")]
    Browser(#[from] BrowserError),
    #[error("ai error: {0}")]
    Ai(#[from] AiError),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn run_error_should_wrap_config_error_via_from() {
        let err: RunError = ConfigError::ThresholdRange(1.5).into();
        assert!(matches!(
            err,
            RunError::Config(ConfigError::ThresholdRange(_))
        ));
    }

    #[test]
    fn run_error_should_wrap_browser_error_via_from() {
        let err: RunError = BrowserError::ChromeNotFound.into();
        assert!(matches!(
            err,
            RunError::Browser(BrowserError::ChromeNotFound)
        ));
    }

    #[test]
    fn run_error_should_wrap_io_error_via_from() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing config");
        let err: RunError = io.into();
        assert!(matches!(err, RunError::Io(_)));
    }

    #[test]
    fn run_error_source_should_chain_to_inner_error() {
        let err: RunError = AiError::RateLimited {
            retry_after_secs: 30,
        }
        .into();
        // `#[from]` sets the wrapped error as the source of the chain
        let source = err.source().expect("RunError::Ai should expose its source");
        assert_eq!(source.to_string(), "rate limited; retry after 30s");
    }

    #[test]
    fn browser_error_timeout_should_display_context() {
        let msg = BrowserError::Timeout {
            target: "input[name=password]".to_string(),
            ms: 5000,
        }
        .to_string();
        assert!(msg.contains("5000ms"));
        assert!(msg.contains("input[name=password]"));
    }

    #[test]
    fn config_error_empty_spec_should_display_check_name() {
        let msg = ConfigError::EmptySpec {
            name: "login".to_string(),
        }
        .to_string();
        assert!(msg.contains("login"));
    }
}

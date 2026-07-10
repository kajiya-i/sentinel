//! Core domain types for Sentinel.
//!
//! Domain values are newtypes (`CheckId`, `TargetUrl`, `Confidence`, `Threshold`) so raw
//! numbers/strings can't be transposed at the crate boundary, and growable public enums
//! (`Verdict`, `Action`) are `#[non_exhaustive]` so adding a variant stays non-breaking
//! (docs/rules/rust.md, docs/rules/design.md).
//!
//! Serde derives, config validation, error types, and port traits are intentionally *not*
//! here — they land in T-M0-07/08/09.

use std::fmt;
use std::path::PathBuf;

/// Outcome of judging a screen against its spec.
///
/// `Fail` is a spec violation; `Error` is an execution failure (page not loaded, timeout).
/// The two are deliberately distinct (docs/specs/core-mechanism.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// Screen satisfies the spec.
    Pass,
    /// Screen violates the spec (see `Judgment::violations`).
    Fail,
    /// Confidence below threshold — routed to human triage.
    NeedsReview,
    /// Execution failure (load/timeout/etc.), not a spec judgment.
    Error,
}

/// The judge's self-reported confidence, constrained to `0.0..=1.0`.
///
/// The value is uncalibrated on its own; it gates `NeedsReview` and escalation but is not
/// trusted in isolation (docs/specs/ai-judgment.md).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Confidence(f32);

impl Confidence {
    /// Returns `None` unless `value` is finite and within `0.0..=1.0`.
    pub fn new(value: f32) -> Option<Self> {
        (value.is_finite() && (0.0..=1.0).contains(&value)).then_some(Self(value))
    }

    /// The wrapped value, guaranteed finite and within `0.0..=1.0`.
    pub fn get(self) -> f32 {
        self.0
    }
}

/// The per-check confidence cutoff: below this, a verdict is downgraded to `NeedsReview`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Threshold(f32);

impl Threshold {
    /// Returns `None` unless `value` is finite and within `0.0..=1.0`.
    pub fn new(value: f32) -> Option<Self> {
        (value.is_finite() && (0.0..=1.0).contains(&value)).then_some(Self(value))
    }

    /// The wrapped value, guaranteed finite and within `0.0..=1.0`.
    pub fn get(self) -> f32 {
        self.0
    }
}

/// Stable identifier for a check (e.g. its file stem). The MVP is stateless, so this is a
/// meaningful string rather than a database-assigned number.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CheckId(String);

impl CheckId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A check's target URL as written in YAML. May be relative to `defaults.base_url`;
/// resolution to an absolute URL happens during config merge (T-M0-08).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetUrl(String);

impl TargetUrl {
    pub fn new(url: impl Into<String>) -> Self {
        Self(url.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Screenshot viewport in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

impl Default for Viewport {
    /// 1280x800 — token-cheap and matches a typical first-view (docs/specs/core-mechanism.md).
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
        }
    }
}

/// A single browser action performed to reach the state under test. MVP set only;
/// targets are an accessible name or a CSS selector (docs/specs/core-mechanism.md).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// Navigate to a URL.
    Goto { url: TargetUrl },
    /// Click an element.
    Click { target: String },
    /// Type `value` into an input.
    Fill { target: String, value: String },
    /// Wait until an element is present.
    WaitFor { target: String },
}

/// One condition under which a screen is verified: the actions that arrange it plus the
/// natural-language spec. Preconditions (mock/cookie/header/…) arrive in M3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    pub name: String,
    pub actions: Vec<Action>,
    pub spec: String,
}

/// A single screen to verify: one URL across one or more scenarios (a bare single-spec
/// check is one scenario, docs/specs/scenarios.md).
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub id: CheckId,
    pub name: String,
    pub url: TargetUrl,
    pub viewport: Viewport,
    /// Capture the full page instead of just the viewport.
    pub full_page: bool,
    pub threshold: Threshold,
    pub scenarios: Vec<Scenario>,
}

/// Objective evidence the browser collects for the judge: a PNG screenshot and the pruned
/// accessibility tree (docs/specs/ai-judgment.md).
///
/// `Debug` reports sizes only — never the raw bytes or tree, which may contain PII and must
/// not reach logs (docs/rules/logging.md).
#[derive(Clone)]
pub struct Evidence {
    pub screenshot_png: Vec<u8>,
    pub a11y_tree: String,
}

impl fmt::Debug for Evidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Evidence")
            .field("screenshot_png_bytes", &self.screenshot_png.len())
            .field("a11y_tree_chars", &self.a11y_tree.len())
            .finish()
    }
}

/// A cited spec violation backing a `Fail` verdict, for auditability
/// (docs/specs/ai-judgment.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub spec_clause: String,
    pub evidence: String,
}

/// The judge's structured output for one scenario.
#[derive(Debug, Clone, PartialEq)]
pub struct Judgment {
    pub verdict: Verdict,
    pub confidence: Confidence,
    pub reasons: Vec<String>,
    pub violations: Vec<Violation>,
}

/// The recorded outcome of running one check: its judgment plus the on-disk screenshot
/// path. `run_id`/timestamps arrive with persistence (Phase 2, docs/specs/data-model.md).
#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    pub check_id: CheckId,
    pub judgment: Judgment,
    pub screenshot_path: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_new_should_accept_value_in_range() {
        assert_eq!(Confidence::new(0.0).map(Confidence::get), Some(0.0));
        assert_eq!(Confidence::new(1.0).map(Confidence::get), Some(1.0));
        assert_eq!(Confidence::new(0.73).map(Confidence::get), Some(0.73));
    }

    #[test]
    fn confidence_new_should_reject_out_of_range_or_non_finite() {
        assert!(Confidence::new(-0.01).is_none());
        assert!(Confidence::new(1.01).is_none());
        assert!(Confidence::new(f32::NAN).is_none());
        assert!(Confidence::new(f32::INFINITY).is_none());
    }

    #[test]
    fn threshold_new_should_reject_out_of_range() {
        assert!(Threshold::new(0.7).is_some());
        assert!(Threshold::new(-1.0).is_none());
        assert!(Threshold::new(2.0).is_none());
    }

    #[test]
    fn viewport_default_should_be_1280x800() {
        assert_eq!(
            Viewport::default(),
            Viewport {
                width: 1280,
                height: 800
            }
        );
    }

    #[test]
    fn check_id_as_str_should_return_inner() {
        assert_eq!(CheckId::new("login").as_str(), "login");
    }

    #[test]
    fn evidence_debug_should_not_leak_raw_bytes() {
        let a11y = "button \"Submit\" disabled";
        let evidence = Evidence {
            screenshot_png: vec![0xDE, 0xAD, 0xBE, 0xEF],
            a11y_tree: a11y.to_string(),
        };
        let rendered = format!("{evidence:?}");
        // sizes are shown, raw payload is not
        assert!(rendered.contains("screenshot_png_bytes: 4"));
        assert!(rendered.contains(&format!("a11y_tree_chars: {}", a11y.len())));
        assert!(!rendered.contains("Submit")); // a11y content not leaked
        assert!(!rendered.contains("222")); // 0xDE as a decimal byte not leaked
    }

    #[test]
    fn check_should_construct_with_scenarios() {
        let check = Check {
            id: CheckId::new("login"),
            name: "login screen".to_string(),
            url: TargetUrl::new("/login"),
            viewport: Viewport::default(),
            full_page: false,
            threshold: Threshold::new(0.7).expect("0.7 is in range"),
            scenarios: vec![Scenario {
                name: "empty email".to_string(),
                actions: vec![Action::WaitFor {
                    target: "input[name=password]".to_string(),
                }],
                spec: "submit button is disabled".to_string(),
            }],
        };
        assert_eq!(check.url.as_str(), "/login");
        assert_eq!(check.scenarios.len(), 1);
    }
}

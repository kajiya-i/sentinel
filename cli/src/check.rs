//! The `sentinel check` command — the M1 walking-skeleton pipeline.
//!
//! Composes the concrete adapters for a single hardcoded abnormal-state case:
//! arrange (mock an API to 500) → collect evidence (screenshot + a11y) → judge against a
//! natural-language spec → print the verdict → return an exit code. Hardcoding is deliberate
//! for M1 (docs/roadmap/M1-walking-skeleton.md); the generic multi-check orchestrator
//! (bounded-parallel, retry, per-check fail-soft) is M5 (T-M5-05), threshold/escalation is M4
//! (T-M4-04), the real `Reporter` is M5 (T-M5-02), and full exit-code / `--strict` handling is
//! M5 (T-M5-04).

use sentinel_ai::ClaudeJudge;
use sentinel_browser::{ChromiumBrowser, LaunchOptions, MockRule};
use sentinel_core::{
    Check, CheckId, CheckResult, Judge, RunError, Scenario, TargetUrl, Threshold, Verdict, Viewport,
};

/// Exit code for a run that couldn't produce a verdict (bad env, launch failure, transport
/// error). Distinct from a verdict-driven non-zero so CI can tell "check failed to run" from
/// "check ran and found a spec violation".
const EXIT_PIPELINE_ERROR: i32 = 2;

/// Run `sentinel check` and return the process exit code.
///
/// Reads `ANTHROPIC_API_KEY` from the environment (never logged). A missing key or a pipeline
/// failure is reported to stderr and mapped to [`EXIT_PIPELINE_ERROR`] — the check never panics.
pub async fn run_and_report() -> i32 {
    let api_key = match std::env::var("ANTHROPIC_API_KEY") {
        Ok(k) if !k.trim().is_empty() => k,
        _ => {
            eprintln!("error: ANTHROPIC_API_KEY is not set");
            return EXIT_PIPELINE_ERROR;
        }
    };

    match run(&api_key).await {
        Ok(result) => {
            print!("{}", format_result(&result));
            i32::from(exit_code(result.judgment.verdict))
        }
        Err(e) => {
            // The error carries no secret (adapter errors are string-mapped, docs/rules/logging.md).
            tracing::error!(error = %e, "check pipeline failed");
            eprintln!("error: check failed: {e}");
            EXIT_PIPELINE_ERROR
        }
    }
}

/// Drive the one hardcoded check end-to-end. A single-check failure surfaces as a [`RunError`]
/// here; mapping per-check failures to `verdict = Error` without aborting a wider run is the
/// suite orchestrator's job (M5, T-M5-05).
async fn run(api_key: &str) -> Result<CheckResult, RunError> {
    let (check, mocks) = hardcoded_check();
    let scenario = &check.scenarios[0];

    // The abnormal state (API → 500) is arranged via the concrete browser API: mock/precondition
    // wiring isn't on the `Browser` port yet (Scenario → mock is M3, T-M3-05), and the CLI is the
    // composition root that may use richer adapter methods.
    // Launch once per run (default: auto-detect Chrome, sandbox on). User-facing chrome-path /
    // sandbox control is M5 (T-M5-01); the adapter is already configurable.
    let browser = ChromiumBrowser::launch(LaunchOptions::default()).await?;
    let evidence = browser.collect_with_mocks(&check, scenario, &mocks).await;
    // Close before judging (the browser isn't needed past evidence collection) so it shuts down
    // cleanly even when collection failed — no "not closed manually" warning, no lingering child.
    browser.close().await;
    let evidence = evidence?;

    let judge = ClaudeJudge::new(api_key);
    let judgment = judge.judge(&scenario.spec, &evidence).await?;

    Ok(CheckResult {
        check_id: check.id.clone(),
        judgment,
        screenshot_path: None, // on-disk artifacts are M5 (T-M5-03)
    })
}

/// The single walking-skeleton case: mock the orders API to 500 and assert the page shows a
/// user-facing error. The URL host is a reserved `.invalid` domain (RFC 2606) — the request is
/// fulfilled by the mock before DNS, so nothing reaches the network.
fn hardcoded_check() -> (Check, Vec<MockRule>) {
    let check = Check {
        id: CheckId::new("orders-500"),
        name: "orders API 500 error screen".to_string(),
        url: TargetUrl::new("http://orders.api.invalid/orders"),
        viewport: Viewport::default(),
        full_page: false,
        // INVARIANT: 0.7 is a valid Threshold (finite, within 0.0..=1.0).
        threshold: Threshold::new(0.7).expect("0.7 is in range"),
        scenarios: vec![Scenario {
            name: "api returns 500".to_string(),
            actions: Vec::new(),
            spec: "When the orders API fails, the page shows a clear server-error message to \
                   the user (for example a heading indicating an error), not a blank page."
                .to_string(),
        }],
    };
    let mocks = vec![MockRule {
        url_substring: "orders.api.invalid/orders".to_string(),
        status: 500,
    }];
    (check, mocks)
}

/// Render a check result as a minimal human summary. The full terminal/JSON reporter is M5
/// (T-M5-02/03); this is just enough to see the verdict end-to-end.
fn format_result(result: &CheckResult) -> String {
    let j = &result.judgment;
    let mut out = format!(
        "check:   {}\nverdict: {:?} (confidence {:.2})\n",
        result.check_id.as_str(),
        j.verdict,
        j.confidence.get(),
    );
    if !j.reasons.is_empty() {
        out.push_str("reasons:\n");
        for r in &j.reasons {
            out.push_str(&format!("  - {r}\n"));
        }
    }
    if !j.violations.is_empty() {
        out.push_str("violations:\n");
        for v in &j.violations {
            out.push_str(&format!("  - [{}] {}\n", v.spec_clause, v.evidence));
        }
    }
    out
}

/// Map a verdict to a process exit code. `pass`/`needs_review` succeed; everything else —
/// `fail`, `error`, and any future variant — fails the build (fail-safe). `--strict` (making
/// `needs_review` non-zero) and distinct codes are M5 (T-M5-04).
fn exit_code(verdict: Verdict) -> u8 {
    match verdict {
        Verdict::Pass | Verdict::NeedsReview => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_core::{Confidence, Judgment, Violation};

    fn result_with(
        verdict: Verdict,
        reasons: Vec<&str>,
        violations: Vec<(&str, &str)>,
    ) -> CheckResult {
        CheckResult {
            check_id: CheckId::new("orders-500"),
            judgment: Judgment {
                verdict,
                confidence: Confidence::new(0.82).expect("0.82 in range"),
                reasons: reasons.into_iter().map(String::from).collect(),
                violations: violations
                    .into_iter()
                    .map(|(c, e)| Violation {
                        spec_clause: c.to_string(),
                        evidence: e.to_string(),
                    })
                    .collect(),
            },
            screenshot_path: None,
        }
    }

    #[test]
    fn exit_code_should_map_pass_and_needs_review_to_zero() {
        assert_eq!(exit_code(Verdict::Pass), 0);
        assert_eq!(exit_code(Verdict::NeedsReview), 0);
    }

    #[test]
    fn exit_code_should_map_fail_and_error_to_nonzero() {
        assert_eq!(exit_code(Verdict::Fail), 1);
        assert_eq!(exit_code(Verdict::Error), 1);
    }

    #[test]
    fn format_result_should_render_verdict_confidence_and_details() {
        let result = result_with(
            Verdict::Fail,
            vec!["no error message shown"],
            vec![("clear server-error message", "page is blank")],
        );
        let out = format_result(&result);
        assert!(out.contains("check:   orders-500"));
        assert!(out.contains("verdict: Fail (confidence 0.82)"));
        assert!(out.contains("  - no error message shown"));
        assert!(out.contains("  - [clear server-error message] page is blank"));
    }

    #[test]
    fn format_result_should_omit_empty_sections() {
        let out = format_result(&result_with(Verdict::Pass, vec![], vec![]));
        assert!(out.contains("verdict: Pass"));
        assert!(!out.contains("reasons:"));
        assert!(!out.contains("violations:"));
    }

    #[test]
    fn hardcoded_check_should_mock_the_target_to_500() {
        let (check, mocks) = hardcoded_check();
        assert_eq!(mocks.len(), 1);
        assert_eq!(mocks[0].status, 500);
        assert!(check.url.as_str().contains(&mocks[0].url_substring));
        assert!(!check.scenarios[0].spec.is_empty());
    }
}

//! Port traits: the seams `core` uses to reach the outside world.
//!
//! `core` depends on these traits, never on the concrete adapters that implement them
//! (`browser`/`ai`/`store` depend on `core` — dependency inversion, docs/rules/design.md §1).
//! Every method returns domain types; no adapter type (reqwest/chromiumoxide/sqlx) appears
//! in a signature. `#[async_trait]` keeps the async ports `dyn`-safe and their futures `Send`
//! (needed for the bounded-parallel run in M1); `Send + Sync` lets an adapter be shared
//! across worker tasks.
//!
//! Method set is minimal for the skeleton; the real orchestration that drives these ports
//! (retry, threshold, verdict mapping, concurrency) lands in M1.

use async_trait::async_trait;

use crate::domain::{Check, CheckResult, Evidence, Judgment, Scenario};
use crate::error::{AiError, BrowserError, StoreError};

/// Opens the target, arranges the scenario, waits for the page to settle, and captures
/// objective [`Evidence`] (screenshot + pruned a11y tree). The browser owns the timing so
/// evidence is trustworthy (docs/specs/ai-judgment.md — evidence-first).
#[async_trait]
pub trait Browser: Send + Sync {
    async fn collect_evidence(
        &self,
        check: &Check,
        scenario: &Scenario,
    ) -> Result<Evidence, BrowserError>;
}

/// Judges a scenario's natural-language `spec` against the collected [`Evidence`] and returns
/// a structured [`Judgment`] (verdict / confidence / violations, docs/specs/ai-judgment.md).
#[async_trait]
pub trait Judge: Send + Sync {
    async fn judge(&self, spec: &str, evidence: &Evidence) -> Result<Judgment, AiError>;
}

/// Persists a [`CheckResult`]. Phase 2 (the MVP runs stateless); defined now so the port
/// surface is complete and [`crate::RunError`] has a consumer for [`StoreError`].
#[async_trait]
pub trait Store: Send + Sync {
    async fn save_result(&self, result: &CheckResult) -> Result<(), StoreError>;
}

/// Renders results for humans and machines. Synchronous: reporting is local output (stdout /
/// JSON file), kept separate from diagnostic logging (docs/rules/logging.md).
pub trait Reporter: Send + Sync {
    fn report(&self, results: &[CheckResult]) -> Result<(), std::io::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Check, CheckId, Confidence, Judgment, Scenario, TargetUrl, Threshold, Verdict, Viewport,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeBrowser;

    #[async_trait]
    impl Browser for FakeBrowser {
        async fn collect_evidence(
            &self,
            _check: &Check,
            _scenario: &Scenario,
        ) -> Result<Evidence, BrowserError> {
            Ok(Evidence {
                screenshot_png: Vec::new(),
                a11y_tree: "button \"Submit\"".to_string(),
            })
        }
    }

    struct FakeJudge;

    #[async_trait]
    impl Judge for FakeJudge {
        async fn judge(&self, _spec: &str, _evidence: &Evidence) -> Result<Judgment, AiError> {
            Ok(Judgment {
                verdict: Verdict::Pass,
                confidence: Confidence::new(0.9).expect("0.9 in range"),
                reasons: Vec::new(),
                violations: Vec::new(),
            })
        }
    }

    #[derive(Default)]
    struct FakeStore {
        saved: AtomicUsize,
    }

    #[async_trait]
    impl Store for FakeStore {
        async fn save_result(&self, _result: &CheckResult) -> Result<(), StoreError> {
            self.saved.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FakeReporter;

    impl Reporter for FakeReporter {
        fn report(&self, _results: &[CheckResult]) -> Result<(), std::io::Error> {
            Ok(())
        }
    }

    fn sample_check() -> Check {
        Check {
            id: CheckId::new("login"),
            name: "login".to_string(),
            url: TargetUrl::new("https://example.com/login"),
            viewport: Viewport::default(),
            full_page: false,
            threshold: Threshold::new(0.7).expect("0.7 in range"),
            scenarios: vec![Scenario {
                name: "default".to_string(),
                actions: Vec::new(),
                spec: "submit button is visible".to_string(),
            }],
        }
    }

    #[tokio::test]
    async fn fake_ports_should_compose_into_a_check_result() {
        let browser = FakeBrowser;
        let judge = FakeJudge;
        let store = FakeStore::default();
        let reporter = FakeReporter;

        let check = sample_check();
        let scenario = &check.scenarios[0];

        // dummy orchestration: collect evidence -> judge -> persist -> report
        let evidence = browser
            .collect_evidence(&check, scenario)
            .await
            .expect("fake evidence");
        let judgment = judge
            .judge(&scenario.spec, &evidence)
            .await
            .expect("fake judgment");
        let result = CheckResult {
            check_id: check.id.clone(),
            judgment,
            screenshot_path: None,
        };
        store.save_result(&result).await.expect("fake save");
        reporter
            .report(std::slice::from_ref(&result))
            .expect("fake report");

        assert_eq!(result.judgment.verdict, Verdict::Pass);
        assert_eq!(store.saved.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ports_should_be_usable_as_trait_objects() {
        // dyn-safety guard: adapters may be injected as boxed trait objects.
        let _browser: Box<dyn Browser> = Box::new(FakeBrowser);
        let _judge: Box<dyn Judge> = Box::new(FakeJudge);
        let _store: Box<dyn Store> = Box::new(FakeStore::default());
        let _reporter: Box<dyn Reporter> = Box::new(FakeReporter);
    }
}

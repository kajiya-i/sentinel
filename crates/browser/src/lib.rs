//! `sentinel-browser` — `Browser` port implementation over chromiumoxide (CDP).
//!
//! Launches Chrome, opens the check's URL, and captures objective evidence: a PNG screenshot
//! and the raw accessibility tree (`Accessibility.getFullAXTree`). This is the M1 minimal
//! path — action execution, condition arrangement (Fetch interception), a11y pruning, precise
//! viewport sizing, and full auto-wait land in M2–M3.

use async_trait::async_trait;
use chromiumoxide::cdp::browser_protocol::accessibility::{
    EnableParams as AxEnableParams, GetFullAxTreeParams,
};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::{Browser as CdpBrowser, BrowserConfig};
use futures::StreamExt;

use sentinel_core::{Browser, BrowserError, Check, Evidence, Scenario};

/// A launched Chrome instance driven over CDP. Reused across checks (each check gets its own
/// page); the CDP event loop runs in a spawned task for the browser's lifetime.
pub struct ChromiumBrowser {
    browser: CdpBrowser,
    // The handler stream must be polled continuously or every CDP call stalls; keep the task
    // alive for the browser's lifetime.
    _handler_task: tokio::task::JoinHandle<()>,
}

impl ChromiumBrowser {
    /// Launch a headless Chrome (auto-detected from the system) and start its CDP event loop.
    pub async fn launch() -> Result<Self, BrowserError> {
        let config = BrowserConfig::builder()
            .build()
            .map_err(BrowserError::Protocol)?;
        // Preserve the real cause (missing binary, sandbox denial, launch timeout, …) rather
        // than collapsing every failure to `ChromeNotFound`, which hides root causes in CI.
        let (browser, mut handler) = CdpBrowser::launch(config)
            .await
            .map_err(|e| BrowserError::Protocol(format!("chrome launch failed: {e}")))?;
        let handler_task = tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if event.is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            browser,
            _handler_task: handler_task,
        })
    }
}

#[async_trait]
impl Browser for ChromiumBrowser {
    async fn collect_evidence(
        &self,
        check: &Check,
        scenario: &Scenario,
    ) -> Result<Evidence, BrowserError> {
        let target = check.url.as_str();
        validate_scheme(target)?;

        if !scenario.actions.is_empty() {
            // Action execution arrives in M2; the minimal path captures the initial page.
            tracing::warn!(
                check = %check.name,
                actions = scenario.actions.len(),
                "scenario actions are not executed yet; capturing the initial page"
            );
        }

        let page = self
            .browser
            .new_page("about:blank")
            .await
            .map_err(|e| BrowserError::Protocol(e.to_string()))?;
        page.goto(target)
            .await
            .map_err(|_| BrowserError::Navigation {
                url: target.to_string(),
            })?;
        // `goto` returns once navigation is *committed*, not once the page has loaded, so a
        // real page would be captured blank. Wait for the load event before taking evidence.
        // Element-level auto-wait (network idle / specific selectors) is still M2.
        page.wait_for_navigation()
            .await
            .map_err(|_| BrowserError::Navigation {
                url: target.to_string(),
            })?;

        let screenshot_png = page
            .screenshot(
                ScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png)
                    .full_page(check.full_page)
                    .build(),
            )
            .await
            .map_err(|e| BrowserError::Protocol(e.to_string()))?;

        page.execute(AxEnableParams::default())
            .await
            .map_err(|e| BrowserError::Protocol(e.to_string()))?;
        let tree = page
            .execute(GetFullAxTreeParams::default())
            .await
            .map_err(|e| BrowserError::Protocol(e.to_string()))?;
        let a11y_tree = serde_json::to_string(&tree.nodes)
            .map_err(|e| BrowserError::Protocol(e.to_string()))?;

        let _ = page.close().await; // best-effort; independent page per check

        Ok(Evidence {
            screenshot_png,
            a11y_tree,
        })
    }
}

/// Reject URL schemes we must not open. `http`/`https` are the real targets; `data:` is
/// allowed for test fixtures. Private/metadata-IP blocking and an injectable policy are
/// Post-MVP (docs/rules/security.md §2 — SSRF), but the scheme gate exists from the start.
fn validate_scheme(target: &str) -> Result<(), BrowserError> {
    let parsed = url::Url::parse(target).map_err(|_| BrowserError::Navigation {
        url: target.to_string(),
    })?;
    match parsed.scheme() {
        "http" | "https" | "data" => Ok(()),
        other => Err(BrowserError::UnsupportedScheme {
            scheme: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_core::{CheckId, TargetUrl, Threshold, Viewport};

    #[test]
    fn validate_scheme_should_reject_non_web_schemes() {
        assert!(validate_scheme("https://example.com").is_ok());
        assert!(validate_scheme("http://example.com").is_ok());
        assert!(validate_scheme("data:text/html,x").is_ok());
        assert!(matches!(
            validate_scheme("file:///etc/passwd"),
            Err(BrowserError::UnsupportedScheme { .. })
        ));
        assert!(matches!(
            validate_scheme("chrome://settings"),
            Err(BrowserError::UnsupportedScheme { .. })
        ));
    }

    fn fixture_check(url: &str) -> Check {
        Check {
            id: CheckId::new("fixture"),
            name: "fixture".to_string(),
            url: TargetUrl::new(url),
            viewport: Viewport::default(),
            full_page: false,
            threshold: Threshold::new(0.7).expect("0.7 in range"),
            scenarios: vec![Scenario {
                name: "default".to_string(),
                actions: Vec::new(),
                spec: "the submit button is visible".to_string(),
            }],
        }
    }

    #[tokio::test]
    async fn collect_evidence_should_capture_png_and_a11y() {
        // Browser test: skip gracefully when no Chrome is available (docs/rules/testing.md).
        let Ok(browser) = ChromiumBrowser::launch().await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        let check = fixture_check(
            "data:text/html,<html><body><button>Submit</button><h1>Hello</h1></body></html>",
        );
        let evidence = browser
            .collect_evidence(&check, &check.scenarios[0])
            .await
            .expect("evidence captured");

        assert!(
            evidence
                .screenshot_png
                .starts_with(&[0x89, b'P', b'N', b'G']),
            "screenshot is not a PNG"
        );
        // The full a11y tree must carry both the role and the accessible name of the button.
        let a11y = evidence.a11y_tree.to_lowercase();
        assert!(a11y.contains("button"), "a11y tree missing the button role");
        assert!(a11y.contains("submit"), "a11y tree missing the button name");
    }
}

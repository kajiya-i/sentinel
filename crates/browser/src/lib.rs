//! `sentinel-browser` — `Browser` port implementation over chromiumoxide (CDP).
//!
//! Launches Chrome, opens the check's URL, and captures objective evidence: a PNG screenshot
//! and the raw accessibility tree (`Accessibility.getFullAXTree`). This is the M1 minimal
//! path — action execution, condition arrangement (Fetch interception), a11y pruning, precise
//! viewport sizing, and full auto-wait land in M2–M3.

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::accessibility::{
    EnableParams as AxEnableParams, GetFullAxTreeParams,
};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FulfillRequestParams, HeaderEntry,
};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::{Browser as CdpBrowser, BrowserConfig};
use futures::StreamExt;
use tokio::task::JoinHandle;

use sentinel_core::{Browser, BrowserError, Check, Evidence, Scenario};

/// A minimal request-mocking rule for the walking skeleton: any intercepted request whose URL
/// contains `url_substring` is fulfilled with `status` instead of reaching the network. The
/// full route DSL (globs, `body_file`, delay, connection failure) is M3.
#[derive(Debug, Clone)]
pub struct MockRule {
    pub url_substring: String,
    pub status: u16,
}

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
        self.collect_with_mocks(check, scenario, &[]).await
    }
}

impl ChromiumBrowser {
    /// Like [`Browser::collect_evidence`], but first installs CDP `Fetch` interception so that
    /// matching requests are short-circuited with a mocked response — the minimal way to
    /// arrange an abnormal state (e.g. an API returning 500). Wiring mocks into
    /// `Scenario` preconditions is M3; for now the caller passes rules explicitly.
    pub async fn collect_with_mocks(
        &self,
        check: &Check,
        scenario: &Scenario,
        mocks: &[MockRule],
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

        // Interception must be running before navigation, or paused requests hang the page.
        let interception = if mocks.is_empty() {
            None
        } else {
            Some(install_mocks(&page, mocks).await?)
        };

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
        if let Some(task) = interception {
            task.abort();
        }

        Ok(Evidence {
            screenshot_png,
            a11y_tree,
        })
    }
}

/// Enable CDP `Fetch` and spawn the `requestPaused` loop: matching requests are fulfilled with
/// their mocked status (Playwright's `page.route` has no chromiumoxide sugar, so the loop is
/// hand-rolled — docs/specs/scenarios.md). Non-matching requests pass through. The returned
/// task must be aborted when the page is done.
async fn install_mocks(page: &Page, mocks: &[MockRule]) -> Result<JoinHandle<()>, BrowserError> {
    let mut paused = page
        .event_listener::<EventRequestPaused>()
        .await
        .map_err(|e| BrowserError::Protocol(e.to_string()))?;
    page.execute(FetchEnableParams::default())
        .await
        .map_err(|e| BrowserError::Protocol(e.to_string()))?;

    let page = page.clone();
    let rules = mocks.to_vec();
    Ok(tokio::spawn(async move {
        while let Some(event) = paused.next().await {
            let request_id = event.request_id.clone();
            match rules
                .iter()
                .find(|r| event.request.url.contains(&r.url_substring))
            {
                Some(rule) => {
                    let body = format!(
                        "<!doctype html><meta charset=utf-8><title>{status}</title><h1>{status} error</h1>",
                        status = rule.status
                    );
                    let mut fulfill =
                        FulfillRequestParams::new(request_id.clone(), i64::from(rule.status));
                    // `body` is a base64 string wrapped in chromiumoxide's `Binary`.
                    fulfill.body = Some(BASE64.encode(body).into());
                    fulfill.response_headers = Some(vec![
                        HeaderEntry::new("content-type", "text/html; charset=utf-8"),
                        // Let cross-origin `fetch()` read the mocked response (M3 scenarios).
                        HeaderEntry::new("access-control-allow-origin", "*"),
                    ]);
                    // If fulfilling fails, release the request so navigation can't hang forever
                    // (`wait_for_navigation` has no timeout) — a stalled check would violate the
                    // fail-soft rule (a check failure must surface, not stop the run).
                    if let Err(e) = page.execute(fulfill).await {
                        tracing::warn!(url = %event.request.url, error = %e, "fetch fulfill failed; releasing request");
                        let _ = page.execute(ContinueRequestParams::new(request_id)).await;
                    }
                }
                None => {
                    if let Err(e) = page.execute(ContinueRequestParams::new(request_id)).await {
                        tracing::warn!(url = %event.request.url, error = %e, "fetch continue failed");
                    }
                }
            }
        }
    }))
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

    #[tokio::test]
    async fn collect_with_mocks_should_render_mocked_500() {
        let Ok(browser) = ChromiumBrowser::launch().await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        // This exercises the interception mechanism (`fulfillRequest`), not a realistic
        // scenario: the 500 is *fabricated* by the mock, independent of the target. The host is
        // a reserved `.invalid` domain (RFC 2606) used only as a fail-closed sink — the request
        // is fulfilled before DNS, so nothing reaches the real network even if the mock breaks.
        // (A non-existent host would naturally give a *network* error, not a 500 — that path is
        // `failRequest`, T-M3-03. Simulating a real server's 500 that an app fetches is M3.)
        let check = fixture_check("http://orders.api.invalid/orders");
        let mocks = [MockRule {
            url_substring: "orders.api.invalid/orders".to_string(),
            status: 500,
        }];
        let evidence = browser
            .collect_with_mocks(&check, &check.scenarios[0], &mocks)
            .await
            .expect("evidence captured");

        let a11y = evidence.a11y_tree.to_lowercase();
        assert!(
            a11y.contains("500") && a11y.contains("error"),
            "mocked 500 error page did not render"
        );
        assert!(
            evidence
                .screenshot_png
                .starts_with(&[0x89, b'P', b'N', b'G']),
            "screenshot is not a PNG"
        );
    }
}

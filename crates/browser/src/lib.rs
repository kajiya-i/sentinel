//! `sentinel-browser` — `Browser` port implementation over chromiumoxide (CDP).
//!
//! Launches Chrome, opens the check's URL, runs the scenario's actions (goto/click/fill/wait_for,
//! targeting accessible-name-first with a CSS fallback), and captures objective evidence: a PNG
//! screenshot and the raw accessibility tree (`Accessibility.getFullAXTree`). Capture waits for
//! the page to settle first — bounded navigation + network-idle + `readyState` (evidence-first).
//! Condition arrangement via CDP `Fetch` interception is wired for the minimal mock case. Still
//! to come: a11y-tree pruning and precise viewport sizing / screenshot downscale (M2,
//! T-M2-04/05), frozen evidence (T-M2-06), and the full route DSL (M3).

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chromiumoxide::cdp::browser_protocol::accessibility::{
    EnableParams as AxEnableParams, GetFullAxTreeParams,
};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FulfillRequestParams, HeaderEntry,
};
use chromiumoxide::cdp::browser_protocol::network::{
    EnableParams as NetworkEnableParams, EventLoadingFailed, EventLoadingFinished,
    EventRequestWillBeSent, RequestId,
};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::listeners::EventStream;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::{Browser as CdpBrowser, BrowserConfig};
use chromiumoxide::{Element, Page};
use futures::StreamExt;
use tokio::task::JoinHandle;

use sentinel_core::{Action, Browser, BrowserError, Check, Evidence, Scenario};

/// A minimal request-mocking rule for the walking skeleton: any intercepted request whose URL
/// contains `url_substring` is fulfilled with `status` instead of reaching the network. The
/// full route DSL (globs, `body_file`, delay, connection failure) is M3.
#[derive(Debug, Clone)]
pub struct MockRule {
    pub url_substring: String,
    pub status: u16,
}

/// How to launch Chrome for a run (docs/roadmap/M2-browser-evidence.md — T-M2-01).
///
/// `#[non_exhaustive]`: more knobs (viewport, launch timeout, …) will be added without breaking
/// callers — construct via `LaunchOptions { .. }` update syntax or `..Default::default()`
/// (docs/rules/design.md §2).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct LaunchOptions {
    /// Explicit Chrome/Chromium executable. `None` auto-detects from the system
    /// (docs/specs/architecture.md — `--chrome-path`).
    pub chrome_path: Option<PathBuf>,
    /// Sandbox policy; see [`SandboxPolicy`].
    pub sandbox: SandboxPolicy,
}

/// Chrome sandbox control. Defaults to [`SandboxPolicy::Enabled`]: the browser opens untrusted
/// pages, so the OS sandbox stays on (docs/rules/security.md §4 — minimal privilege).
/// [`SandboxPolicy::Disabled`] adds `--no-sandbox`, which is required under root/containers where
/// the sandbox can't initialize but removes a real isolation layer — hence an explicit opt-out
/// only. Wiring it to a user-facing flag / config is M5 (T-M5-01).
///
/// `#[non_exhaustive]`: a future `Auto` (disable under root/containers automatically) is likely
/// (docs/rules/design.md §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SandboxPolicy {
    /// Sandbox on (secure default).
    #[default]
    Enabled,
    /// Sandbox off (`--no-sandbox`) for root/container environments.
    Disabled,
}

/// A launched Chrome instance driven over CDP. Reused across checks (each check gets its own
/// page); the CDP event loop runs in a spawned task for the browser's lifetime.
pub struct ChromiumBrowser {
    browser: CdpBrowser,
    // The handler stream must be polled continuously or every CDP call stalls; keep the task
    // alive for the browser's lifetime and abort it in `close`.
    handler_task: tokio::task::JoinHandle<()>,
}

impl ChromiumBrowser {
    /// Launch a headless Chrome and start its CDP event loop. The executable is auto-detected
    /// from the system unless [`LaunchOptions::chrome_path`] overrides it; the sandbox stays on
    /// unless [`LaunchOptions::sandbox`] opts out. Reuse one instance per run — each check gets
    /// its own page (docs/rules/perf.md).
    pub async fn launch(options: LaunchOptions) -> Result<Self, BrowserError> {
        let mut builder = BrowserConfig::builder();
        if let Some(ref path) = options.chrome_path {
            // A missing explicit binary is a detection failure. A path that *exists* but won't
            // launch (e.g. sandbox denial under root, port conflict, CDP handshake) is NOT
            // "not found" — it surfaces below with its real cause, so the operator is pointed at
            // `--no-sandbox` instead of a misleading "chrome not found".
            if !path.exists() {
                return Err(BrowserError::ChromeNotFound);
            }
            builder = builder.chrome_executable(path);
        }
        if options.sandbox == SandboxPolicy::Disabled {
            builder = builder.no_sandbox();
        }
        // `build` runs Chrome auto-detection when no explicit path is set; its only failure mode
        // is "no Chrome found" — a typed detection failure (docs/roadmap/M2 — T-M2-01).
        let config = builder.build().map_err(|_| BrowserError::ChromeNotFound)?;

        // Preserve the real launch cause (sandbox denial, launch timeout, CDP handshake, …)
        // rather than collapsing it to `ChromeNotFound`, which hides root causes (RK-005).
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
            handler_task,
        })
    }

    /// Shut the browser down cleanly at the end of a run. Best-effort — errors are logged, not
    /// returned. Order matters (RK-005): `close` needs the handler task still polling to receive
    /// the CDP response; `wait` then reaps the child process (so chromiumoxide's `Browser::drop`
    /// sees it exited and doesn't warn "not closed manually" / fall back to `kill_on_drop`);
    /// only then is the handler task aborted.
    pub async fn close(mut self) {
        if let Err(e) = self.browser.close().await {
            tracing::warn!(error = %e, "browser close failed");
        }
        // `wait` awaits the child directly and doesn't need the handler task.
        let _ = self.browser.wait().await;
        self.handler_task.abort();
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
        validate_scheme(check.url.as_str())?;

        let page = self
            .browser
            .new_page("about:blank")
            .await
            .map_err(|e| BrowserError::Protocol(e.to_string()))?;

        // Interception must be running before navigation, or paused requests hang the page.
        let interception = if mocks.is_empty() {
            None
        } else {
            match install_mocks(&page, mocks).await {
                Ok(task) => Some(task),
                // Close the page we just opened before bailing (#66 — cleanup on every path).
                Err(e) => {
                    let _ = page.close().await;
                    return Err(e);
                }
            }
        };

        // Navigate, run the scenario's actions, and capture — then ALWAYS release the page and
        // abort the interception task, on success and on error alike (#66). A leaked page/task
        // would accumulate once the browser is reused across checks (M5 suite, T-M5-05).
        let result = capture(&page, check, scenario).await;
        let _ = page.close().await;
        if let Some(task) = interception {
            task.abort();
        }
        result
    }
}

/// Default budget for a `WaitFor` action before giving up with [`BrowserError::Timeout`].
const WAIT_FOR_TIMEOUT: Duration = Duration::from_millis(5_000);
/// Poll interval while a `WaitFor` action waits for its target to appear.
const WAIT_FOR_POLL: Duration = Duration::from_millis(100);
/// Attribute/selector bridging an accessible-name match to a CSS-findable element.
const MARKER_SELECTOR: &str = "[data-sentinel-target]";

/// Overall budget for a single navigation (`goto` + load). chromiumoxide's `wait_for_navigation`
/// has no timeout of its own, so this bounds it (RK-002/RK-003). Fixed for M2; user config is M5.
const NAV_TIMEOUT: Duration = Duration::from_secs(15);
/// Overall budget for waiting on network-idle before capturing evidence.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(10);
/// In-flight requests must stay `<= NETWORK_IDLE_THRESHOLD` this long to count as network-idle.
const NETWORK_QUIET: Duration = Duration::from_millis(500);
/// Playwright-style network-idle threshold: `<= 2` in-flight tolerates keep-alive/analytics that
/// never fully drain to zero.
const NETWORK_IDLE_THRESHOLD: usize = 2;

/// Navigate to the check URL, run the scenario's actions to reach the state under test, then
/// capture evidence (screenshot + full a11y tree). Fallible on its own so
/// [`ChromiumBrowser::collect_with_mocks`] can clean up regardless of the outcome.
async fn capture(
    page: &Page,
    check: &Check,
    scenario: &Scenario,
) -> Result<Evidence, BrowserError> {
    // Start counting network activity *before* navigating: `wait_for_navigation` resolves at the
    // `load` event, not network-idle, and CDP `Network.enable` doesn't replay requests that began
    // before it. Enabling here means a fetch kicked off on load (the async-data / error-UI case
    // this feature exists for) is observed, instead of being missed and captured mid-flight.
    let streams = enable_network(page).await?;

    navigate(page, check.url.as_str()).await?;

    for action in &scenario.actions {
        execute_action(page, action).await?;
    }

    // Settle before capturing: evidence is only ground truth if taken after the page is quiet
    // (docs/specs/ai-judgment.md — evidence-first). This also covers a navigating click (a click
    // in `execute_action` isn't awaited there; the settle here waits for the new page).
    wait_for_settled(page, streams, SETTLE_TIMEOUT).await?;

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
    let a11y_tree =
        serde_json::to_string(&tree.nodes).map_err(|e| BrowserError::Protocol(e.to_string()))?;

    Ok(Evidence {
        screenshot_png,
        a11y_tree,
    })
}

/// Navigate to `url` and wait for the load event, bounded by [`NAV_TIMEOUT`]. `goto` returns on
/// navigation *commit*, not load, so `wait_for_navigation` is required (RK-002); neither has a
/// timeout of its own, so the whole step is wrapped (RK-003). Elapse → [`BrowserError::Timeout`].
async fn navigate(page: &Page, url: &str) -> Result<(), BrowserError> {
    let nav = async {
        page.goto(url).await.map_err(|_| BrowserError::Navigation {
            url: url.to_string(),
        })?;
        page.wait_for_navigation()
            .await
            .map_err(|_| BrowserError::Navigation {
                url: url.to_string(),
            })?;
        Ok::<(), BrowserError>(())
    };
    match tokio::time::timeout(NAV_TIMEOUT, nav).await {
        Ok(result) => result,
        Err(_elapsed) => Err(BrowserError::Timeout {
            target: url.to_string(),
            ms: NAV_TIMEOUT.as_millis() as u64,
        }),
    }
}

/// The CDP `Network` event streams used to detect network-idle, produced by [`enable_network`]
/// before navigation and consumed by [`wait_for_settled`] afterwards.
struct NetworkStreams {
    sent: EventStream<EventRequestWillBeSent>,
    finished: EventStream<EventLoadingFinished>,
    failed: EventStream<EventLoadingFailed>,
}

/// Register the `Network` request listeners and enable the domain. Call this *before* navigating:
/// `Network.enable` does not replay requests that began earlier, so a fetch started on `load`
/// would otherwise be invisible to the settle loop. Listeners are registered before `enable`, or
/// early events are missed (RK-003).
async fn enable_network(page: &Page) -> Result<NetworkStreams, BrowserError> {
    let sent = page
        .event_listener::<EventRequestWillBeSent>()
        .await
        .map_err(|e| BrowserError::Protocol(e.to_string()))?;
    let finished = page
        .event_listener::<EventLoadingFinished>()
        .await
        .map_err(|e| BrowserError::Protocol(e.to_string()))?;
    let failed = page
        .event_listener::<EventLoadingFailed>()
        .await
        .map_err(|e| BrowserError::Protocol(e.to_string()))?;
    page.execute(NetworkEnableParams::default())
        .await
        .map_err(|e| BrowserError::Protocol(e.to_string()))?;
    Ok(NetworkStreams {
        sent,
        finished,
        failed,
    })
}

/// Wait until the page is settled — network-idle (`<= NETWORK_IDLE_THRESHOLD` in-flight requests
/// sustained for [`NETWORK_QUIET`]) *and* `document.readyState === "complete"` — bounded by
/// `timeout`, over the streams [`enable_network`] opened before navigation. This is the
/// evidence-first gate: capturing before the async data/error UI has rendered yields false
/// verdicts. Elapse → [`BrowserError::Timeout`] (a never-idle page fails fast instead of hanging —
/// "critical for timeout scenarios"). Coexists with `install_mocks`: a fulfilled request still
/// fires `requestWillBeSent` then `loadingFinished`, so it leaves the in-flight set.
async fn wait_for_settled(
    page: &Page,
    mut streams: NetworkStreams,
    timeout: Duration,
) -> Result<(), BrowserError> {
    let settle = async {
        // Track in-flight requests by `RequestId` rather than a counter: a redirect re-fires
        // `requestWillBeSent` under the *same* id (idempotent insert) but finishes only once, and
        // an id we never saw start (began before enable) just misses on removal — both of which a
        // bare +1/-1 counter would mishandle.
        let mut in_flight: HashSet<RequestId> = HashSet::new();
        loop {
            tokio::select! {
                ev = streams.sent.next() => match ev {
                    Some(e) => { in_flight.insert(e.request_id.clone()); }
                    None => return Ok(()), // stream ended (page gone) — nothing left to wait on
                },
                ev = streams.finished.next() => match ev {
                    Some(e) => { in_flight.remove(&e.request_id); }
                    None => return Ok(()),
                },
                ev = streams.failed.next() => match ev {
                    Some(e) => { in_flight.remove(&e.request_id); }
                    None => return Ok(()),
                },
                // Quiet window: only armed while the network is idle. A new request restarts it.
                _ = tokio::time::sleep(NETWORK_QUIET), if in_flight.len() <= NETWORK_IDLE_THRESHOLD => {
                    if document_ready(page).await? {
                        return Ok(());
                    }
                }
            }
        }
    };

    match tokio::time::timeout(timeout, settle).await {
        Ok(result) => result,
        Err(_elapsed) => Err(BrowserError::Timeout {
            target: "network-idle".to_string(),
            ms: timeout.as_millis() as u64,
        }),
    }
}

/// Whether the document has finished parsing (`readyState === "complete"`).
async fn document_ready(page: &Page) -> Result<bool, BrowserError> {
    page.evaluate("document.readyState === \"complete\"")
        .await
        .map_err(|e| BrowserError::Protocol(e.to_string()))?
        .into_value()
        .map_err(|e| BrowserError::Protocol(e.to_string()))
}

/// Execute one scenario action against the current page (docs/specs/core-mechanism.md — the MVP
/// action set). Targets resolve accessible-name-first with a CSS-selector fallback ([`resolve`]).
async fn execute_action(page: &Page, action: &Action) -> Result<(), BrowserError> {
    match action {
        Action::Goto { url } => {
            let u = url.as_str();
            validate_scheme(u)?;
            navigate(page, u).await?;
        }
        Action::Click { target } => {
            resolve(page, target)
                .await?
                .click()
                .await
                .map_err(|e| BrowserError::Protocol(e.to_string()))?;
            // A click that triggers navigation isn't awaited here: bounded per-action settling
            // (network idle / load) is auto-wait, M2 T-M2-03 (#17). Until then a `WaitFor` after
            // a navigating click is the explicit synchronization point.
        }
        Action::Fill { target, value } => {
            let el = resolve(page, target).await?;
            // `focus` (not click) avoids firing an unrelated click handler; `type_str` appends —
            // clearing an existing value first is a later refinement (M2 fixtures start empty).
            el.focus()
                .await
                .map_err(|e| BrowserError::Protocol(e.to_string()))?;
            el.type_str(value)
                .await
                .map_err(|e| BrowserError::Protocol(e.to_string()))?;
        }
        Action::WaitFor { target } => {
            wait_for_target(page, target, WAIT_FOR_TIMEOUT).await?;
        }
        // `Action` is `#[non_exhaustive]`; a variant added later has no execution path here yet.
        _ => {
            return Err(BrowserError::Protocol(
                "unsupported action variant".to_string(),
            ));
        }
    }
    Ok(())
}

/// Poll for a CSS `target` until it appears or `timeout` elapses ([`BrowserError::Timeout`]).
///
/// `WaitFor` matches by CSS selector only — existence-waiting is a selector operation, and using
/// the accessible-name path here would re-scan the whole DOM and mutate it (the marker attribute)
/// on every poll (RK-006). Accessible-name-based waiting is a later refinement; interaction
/// targeting (`resolve`, used by Click/Fill) keeps accessible-name-first.
async fn wait_for_target(page: &Page, target: &str, timeout: Duration) -> Result<(), BrowserError> {
    tokio::time::timeout(timeout, async {
        loop {
            if page.find_element(target).await.is_ok() {
                return;
            }
            tokio::time::sleep(WAIT_FOR_POLL).await;
        }
    })
    .await
    .map_err(|_| BrowserError::Timeout {
        target: target.to_string(),
        ms: timeout.as_millis() as u64,
    })
}

/// Resolve a target string to an element: accessible-name first, then CSS selector
/// (docs/specs/core-mechanism.md — "accessible name を第一、CSS も可"). chromiumoxide only hands
/// out element handles via CSS, so an accessible-name match is bridged through a temporary marker
/// attribute ([`resolve_by_accessible_name`]).
async fn resolve(page: &Page, target: &str) -> Result<Element, BrowserError> {
    if let Some(el) = resolve_by_accessible_name(page, target).await? {
        return Ok(el);
    }
    page.find_element(target)
        .await
        .map_err(|_| BrowserError::ElementNotFound {
            target: target.to_string(),
        })
}

/// Mark the first element whose approximate accessible name equals `target` and return a handle to
/// it, or `None` if nothing matches (so the caller falls back to CSS). The accessible name is
/// approximated (aria-label / associated `<label>` / text / placeholder / title / alt / value);
/// the full ARIA accname algorithm is a later refinement.
///
/// The mark (`evaluate`) and the handle lookup (`find_element`) are two CDP round-trips, so this is
/// not atomic against a mutating page (RK-006): a hostile page could re-target the marker in the
/// gap. Acceptable for M2 (self-owned targets, non-secret input); a single-round-trip resolution is
/// required before M3 fills credentials.
async fn resolve_by_accessible_name(
    page: &Page,
    target: &str,
) -> Result<Option<Element>, BrowserError> {
    let matched: bool = page
        .evaluate(accname_probe_js(target)?)
        .await
        .map_err(|e| BrowserError::Protocol(e.to_string()))?
        .into_value()
        .map_err(|e| BrowserError::Protocol(e.to_string()))?;
    if !matched {
        return Ok(None);
    }
    // The marker matched but the element is gone (mutated away between round-trips) → not found.
    page.find_element(MARKER_SELECTOR)
        .await
        .map(Some)
        .map_err(|_| BrowserError::ElementNotFound {
            target: target.to_string(),
        })
}

/// Build the injection-safe JS expression that marks the first element whose approximate
/// accessible name equals `target` and returns whether one was found. `target` is embedded via
/// `serde_json` string escaping, so a hostile target string can't break out of the literal.
fn accname_probe_js(target: &str) -> Result<String, BrowserError> {
    let name = serde_json::to_string(target).map_err(|e| BrowserError::Protocol(e.to_string()))?;
    Ok(format!(
        r#"(() => {{
  const name = {name};
  const accName = (el) => {{
    const aria = el.getAttribute('aria-label'); if (aria) return aria.trim();
    if (el.labels && el.labels.length) return (el.labels[0].textContent || '').trim();
    const ph = el.getAttribute('placeholder'); if (ph) return ph.trim();
    const title = el.getAttribute('title'); if (title) return title.trim();
    const alt = el.getAttribute('alt'); if (alt) return alt.trim();
    const txt = (el.textContent || '').trim(); if (txt) return txt;
    if ('value' in el && el.value) return String(el.value).trim();
    return '';
  }};
  document.querySelectorAll('[data-sentinel-target]').forEach(e => e.removeAttribute('data-sentinel-target'));
  for (const el of document.querySelectorAll('a,button,input,textarea,select,[role],[aria-label]')) {{
    if (accName(el) === name) {{ el.setAttribute('data-sentinel-target', ''); return true; }}
  }}
  return false;
}})()"#
    ))
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

    /// Bound a browser call so a stalled CDP request fails the test instead of hanging CI
    /// (docs/rules/testing.md; #58). `wait_for_navigation` has no timeout of its own — RK-003.
    const CDP_TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    #[tokio::test]
    async fn launch_should_type_error_for_bogus_chrome_path() {
        // No Chrome needed: an explicit non-existent path is stored, then the spawn fails, and
        // the failure is mapped to a typed `ChromeNotFound` (never a panic). Covers #57's path
        // override + #15's "detection failure is typed".
        let options = LaunchOptions {
            chrome_path: Some(PathBuf::from("/nonexistent/definitely-not-chrome")),
            ..Default::default()
        };
        assert!(matches!(
            ChromiumBrowser::launch(options).await,
            Err(BrowserError::ChromeNotFound)
        ));
    }

    #[tokio::test]
    async fn collect_evidence_should_capture_png_and_a11y() {
        // Browser test: skip gracefully when no Chrome is available (docs/rules/testing.md).
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        let check = fixture_check(
            "data:text/html,<html><body><button>Submit</button><h1>Hello</h1></body></html>",
        );
        let evidence = tokio::time::timeout(
            CDP_TEST_TIMEOUT,
            browser.collect_evidence(&check, &check.scenarios[0]),
        )
        .await
        .expect("collect_evidence timed out")
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

        browser.close().await;
    }

    #[tokio::test]
    async fn collect_with_mocks_should_render_mocked_500() {
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
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
        let evidence = tokio::time::timeout(
            CDP_TEST_TIMEOUT,
            browser.collect_with_mocks(&check, &check.scenarios[0], &mocks),
        )
        .await
        .expect("collect_with_mocks timed out")
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

        browser.close().await;
    }

    #[tokio::test]
    async fn collect_should_reuse_browser_across_multiple_pages() {
        // One instance per run, a fresh page per check (docs/rules/perf.md; #15 reuse).
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        for html in ["<h1>One</h1>", "<h1>Two</h1>"] {
            let check = fixture_check(&format!("data:text/html,{html}"));
            let evidence = tokio::time::timeout(
                CDP_TEST_TIMEOUT,
                browser.collect_evidence(&check, &check.scenarios[0]),
            )
            .await
            .expect("collect_evidence timed out")
            .expect("evidence captured");
            assert!(
                evidence
                    .screenshot_png
                    .starts_with(&[0x89, b'P', b'N', b'G']),
                "screenshot is not a PNG"
            );
        }

        browser.close().await;
    }

    // ---- Actions (#16) ----

    fn check_with_actions(url: &str, actions: Vec<Action>) -> Check {
        Check {
            id: CheckId::new("fixture"),
            name: "fixture".to_string(),
            url: TargetUrl::new(url),
            viewport: Viewport::default(),
            full_page: false,
            threshold: Threshold::new(0.7).expect("0.7 in range"),
            scenarios: vec![Scenario {
                name: "default".to_string(),
                actions,
                spec: "reached the target state".to_string(),
            }],
        }
    }

    #[test]
    fn accname_probe_js_should_escape_target_string() {
        // A hostile target must be embedded as an escaped JS string literal, not break out of it.
        let js = accname_probe_js("a\"b").expect("built js");
        assert!(
            js.contains(r#"const name = "a\"b""#),
            "quote was not JSON-escaped: {js}"
        );
        // Backslash and newline must also be escaped (delegated to serde_json).
        let js2 = accname_probe_js("a\\b\nc").expect("built js");
        assert!(
            js2.contains(r#"const name = "a\\b\nc""#),
            "backslash/newline not escaped: {js2}"
        );
    }

    #[tokio::test]
    async fn goto_action_should_navigate_and_validate_scheme() {
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        // Start on page A, then a `goto` action navigates to page B; evidence must reflect B.
        let check = check_with_actions(
            "data:text/html,<h1>page-A</h1>",
            vec![Action::Goto {
                url: TargetUrl::new("data:text/html,<h1>page-B</h1>"),
            }],
        );
        let evidence = tokio::time::timeout(
            CDP_TEST_TIMEOUT,
            browser.collect_evidence(&check, &check.scenarios[0]),
        )
        .await
        .expect("collect timed out")
        .expect("evidence captured");
        browser.close().await;
        let a11y = evidence.a11y_tree.to_lowercase();
        assert!(a11y.contains("page-b"), "goto did not navigate to B");
        assert!(!a11y.contains("page-a"), "still on A after goto");
    }

    #[tokio::test]
    async fn goto_action_should_reject_non_web_scheme() {
        // The scheme gate fires on action URLs too (SSRF/scheme, docs/rules/security.md §2).
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        let page = browser.browser.new_page("about:blank").await.expect("page");
        let result = execute_action(
            &page,
            &Action::Goto {
                url: TargetUrl::new("file:///etc/passwd"),
            },
        )
        .await;
        let _ = page.close().await;
        browser.close().await;
        assert!(matches!(
            result,
            Err(BrowserError::UnsupportedScheme { .. })
        ));
    }

    #[tokio::test]
    async fn click_action_should_change_page_state() {
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        // The button (resolved by accessible name "Go") flips #o from "todo" to "DONE" on click.
        let check = check_with_actions(
            "data:text/html,<button id=b>Go</button><p id=o>todo</p>\
             <script>document.getElementById('b').onclick=()=>{document.getElementById('o').textContent='DONE'}</script>",
            vec![Action::Click {
                target: "Go".to_string(),
            }],
        );
        let evidence = tokio::time::timeout(
            CDP_TEST_TIMEOUT,
            browser.collect_evidence(&check, &check.scenarios[0]),
        )
        .await
        .expect("collect timed out")
        .expect("evidence captured");
        browser.close().await;
        assert!(
            evidence.a11y_tree.to_lowercase().contains("done"),
            "click did not update the page state"
        );
    }

    #[tokio::test]
    async fn fill_action_should_set_value_by_name_and_css() {
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        // Two inputs mirror their value into #o on input. Fill one by accessible name (its
        // <label>), the other by CSS selector.
        let check = check_with_actions(
            "data:text/html,<label for=e1>Email</label><input id=e1><input id=e2><p id=o></p>\
             <script>function m(){document.getElementById('o').textContent=document.getElementById('e1').value+'|'+document.getElementById('e2').value}\
             document.getElementById('e1').oninput=m;document.getElementById('e2').oninput=m</script>",
            vec![
                Action::Fill {
                    target: "Email".to_string(),
                    value: "a@b.com".to_string(),
                },
                Action::Fill {
                    target: "#e2".to_string(),
                    value: "xyz".to_string(),
                },
            ],
        );
        let evidence = tokio::time::timeout(
            CDP_TEST_TIMEOUT,
            browser.collect_evidence(&check, &check.scenarios[0]),
        )
        .await
        .expect("collect timed out")
        .expect("evidence captured");
        let a11y = evidence.a11y_tree.to_lowercase();
        browser.close().await;
        assert!(a11y.contains("a@b.com"), "fill-by-accessible-name failed");
        assert!(a11y.contains("xyz"), "fill-by-css-selector failed");
    }

    #[tokio::test]
    async fn wait_for_action_should_find_delayed_element() {
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        // The <div> is appended after 150ms; `wait_for` must poll until it exists.
        let check = check_with_actions(
            "data:text/html,<p id=o>waiting</p>\
             <script>setTimeout(function(){var d=document.createElement('div');d.textContent='arrived';document.body.appendChild(d)},150)</script>",
            vec![Action::WaitFor {
                target: "div".to_string(),
            }],
        );
        let evidence = tokio::time::timeout(
            CDP_TEST_TIMEOUT,
            browser.collect_evidence(&check, &check.scenarios[0]),
        )
        .await
        .expect("collect timed out")
        .expect("evidence captured");
        browser.close().await;
        assert!(
            evidence.a11y_tree.to_lowercase().contains("arrived"),
            "wait_for did not wait for the delayed element"
        );
    }

    #[tokio::test]
    async fn wait_for_target_should_timeout_when_absent() {
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        // Drive `wait_for_target` directly with a short budget so the timeout path is fast.
        let page = tokio::time::timeout(
            CDP_TEST_TIMEOUT,
            browser.browser.new_page("data:text/html,<p>x</p>"),
        )
        .await
        .expect("new_page timed out")
        .expect("page");
        let result = wait_for_target(&page, "#never", Duration::from_millis(100)).await;
        let _ = page.close().await;
        browser.close().await;
        assert!(matches!(result, Err(BrowserError::Timeout { .. })));
    }

    #[tokio::test]
    async fn click_action_should_error_when_target_absent() {
        // A missing target is a typed `ElementNotFound`, not a panic — the fail-soft path so the
        // check becomes verdict=error rather than aborting the run.
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        let check = check_with_actions(
            "data:text/html,<p>no button here</p>",
            vec![Action::Click {
                target: "#nope".to_string(),
            }],
        );
        let result = tokio::time::timeout(
            CDP_TEST_TIMEOUT,
            browser.collect_evidence(&check, &check.scenarios[0]),
        )
        .await
        .expect("collect timed out");
        browser.close().await;
        assert!(matches!(result, Err(BrowserError::ElementNotFound { .. })));
    }

    // ---- auto-wait / settle (#17) ----

    #[tokio::test]
    async fn wait_for_settled_should_return_when_network_quiets() {
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        let page = tokio::time::timeout(CDP_TEST_TIMEOUT, browser.browser.new_page("about:blank"))
            .await
            .expect("new_page timed out")
            .expect("page");
        let streams = enable_network(&page).await.expect("enable network");
        navigate(&page, "data:text/html,<h1>static</h1>")
            .await
            .expect("nav");
        // No network activity → settles within the quiet window, well under the budget.
        let result = tokio::time::timeout(
            CDP_TEST_TIMEOUT,
            wait_for_settled(&page, streams, SETTLE_TIMEOUT),
        )
        .await
        .expect("settle timed out");
        let _ = page.close().await;
        browser.close().await;
        assert!(result.is_ok(), "static page did not settle: {result:?}");
    }

    #[tokio::test]
    async fn wait_for_settled_should_time_out_with_tiny_budget() {
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        let page = tokio::time::timeout(CDP_TEST_TIMEOUT, browser.browser.new_page("about:blank"))
            .await
            .expect("new_page timed out")
            .expect("page");
        let streams = enable_network(&page).await.expect("enable network");
        navigate(&page, "data:text/html,<h1>x</h1>")
            .await
            .expect("nav");
        // 1ms is far below the 500ms quiet window, so settling can't complete in the budget.
        let result = wait_for_settled(&page, streams, Duration::from_millis(1)).await;
        let _ = page.close().await;
        browser.close().await;
        assert!(matches!(result, Err(BrowserError::Timeout { .. })));
    }

    #[tokio::test]
    async fn auto_wait_should_capture_post_load_async_content() {
        let Ok(browser) = ChromiumBrowser::launch(LaunchOptions::default()).await else {
            eprintln!("skipping: no chrome available");
            return;
        };
        // Content appended ~100ms after load; the 500ms settle window includes it, so capturing
        // at raw load would miss it.
        let check = check_with_actions(
            "data:text/html,<p>loading</p>\
             <script>setTimeout(function(){var p=document.createElement('p');p.textContent='late-content';document.body.appendChild(p)},100)</script>",
            Vec::new(),
        );
        let evidence = tokio::time::timeout(
            CDP_TEST_TIMEOUT,
            browser.collect_evidence(&check, &check.scenarios[0]),
        )
        .await
        .expect("collect timed out")
        .expect("evidence captured");
        browser.close().await;
        assert!(
            evidence.a11y_tree.to_lowercase().contains("late-content"),
            "auto-wait did not include post-load content"
        );
    }
}

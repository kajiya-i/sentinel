//! Config parsing, validation, and merge for `sentinel.yaml` and check YAML files.
//!
//! Raw YAML is deserialized into serde DTOs (with `deny_unknown_fields` so typos are caught
//! at the boundary), then validated and merged with project `defaults` into the domain
//! [`Check`] (docs/rules/design.md §3). The domain types stay serde-free; all serde and the
//! validating conversions live here. YAML parsing uses `serde_norway` (a maintained
//! `serde_yaml` fork; the original is unmaintained, RUSTSEC-2024-0370).

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::Deserialize;
use serde::de::{self, MapAccess, Visitor};
use url::Url;

use crate::domain::{Action, Check, CheckId, Scenario, TargetUrl, Threshold, Viewport};
use crate::error::ConfigError;

/// Fallback threshold when neither the check nor `defaults` specify one.
const DEFAULT_THRESHOLD: f32 = 0.7;

fn default_concurrency() -> usize {
    4
}

fn default_on_error() -> u32 {
    1
}

/// Project configuration parsed from `sentinel.yaml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default)]
    pub retry: Retry,
    #[serde(default)]
    pub suites: BTreeMap<String, Vec<PathBuf>>,
}

impl ProjectConfig {
    /// Parse and validate a `sentinel.yaml` document. Unknown fields are rejected and
    /// `concurrency` must be at least 1 (0 would stall the orchestrator).
    pub fn from_yaml(text: &str) -> Result<Self, ConfigError> {
        let cfg: Self =
            serde_norway::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        if cfg.concurrency == 0 {
            return Err(ConfigError::Invalid {
                name: "sentinel.yaml".to_string(),
                reason: "concurrency must be >= 1".to_string(),
            });
        }
        Ok(cfg)
    }
}

/// Project-wide defaults that individual checks inherit and may override.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Base URL that relative check URLs resolve against.
    pub base_url: Option<String>,
    pub viewport: Option<ViewportDto>,
    pub threshold: Option<f32>,
    /// Default judge model (consumed by the ai adapter in M4; not part of `Check`).
    pub model: Option<String>,
}

/// Retry policy. Only `error` verdicts retry (never `fail`), at most `on_error` times.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    #[serde(default = "default_on_error")]
    pub on_error: u32,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            on_error: default_on_error(),
        }
    }
}

/// Viewport as written in YAML. Converted into the domain [`Viewport`] during merge so the
/// domain type stays serde-free.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewportDto {
    pub width: u32,
    pub height: u32,
}

impl From<ViewportDto> for Viewport {
    fn from(v: ViewportDto) -> Self {
        Viewport {
            width: v.width,
            height: v.height,
        }
    }
}

/// A check file. `spec` (single-scenario sugar) and `scenarios` are mutually exclusive.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckFileDto {
    name: Option<String>,
    url: String,
    viewport: Option<ViewportDto>,
    #[serde(default)]
    full_page: bool,
    threshold: Option<f32>,
    #[serde(default)]
    actions: Vec<ActionDto>,
    spec: Option<String>,
    #[serde(default)]
    scenarios: Vec<ScenarioDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioDto {
    name: Option<String>,
    #[serde(default)]
    actions: Vec<ActionDto>,
    spec: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GotoArgs {
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClickArgs {
    target: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FillArgs {
    target: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitForArgs {
    target: String,
}

/// One action, written as a single-key map: `fill: { target, value }`. serde_norway
/// (serde_yaml 0.9) represents externally tagged enums with YAML `!tags`, not single-key
/// maps, so the spec's `fill:` form is parsed by hand here — which also lets each args
/// struct reject unknown fields.
#[derive(Debug)]
enum ActionDto {
    Goto(GotoArgs),
    Click(ClickArgs),
    Fill(FillArgs),
    WaitFor(WaitForArgs),
}

impl<'de> Deserialize<'de> for ActionDto {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ActionVisitor;

        impl<'de> Visitor<'de> for ActionVisitor {
            type Value = ActionDto;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a single-key action map like `fill: { target, value }`")
            }

            fn visit_map<A>(self, mut map: A) -> Result<ActionDto, A::Error>
            where
                A: MapAccess<'de>,
            {
                let key: String = map
                    .next_key()?
                    .ok_or_else(|| de::Error::custom("empty action item"))?;
                let action = match key.as_str() {
                    "goto" => ActionDto::Goto(map.next_value()?),
                    "click" => ActionDto::Click(map.next_value()?),
                    "fill" => ActionDto::Fill(map.next_value()?),
                    "wait_for" => ActionDto::WaitFor(map.next_value()?),
                    other => return Err(de::Error::custom(format!("unknown action `{other}`"))),
                };
                if map.next_key::<String>()?.is_some() {
                    return Err(de::Error::custom(
                        "an action item must have exactly one action key",
                    ));
                }
                Ok(action)
            }
        }

        deserializer.deserialize_map(ActionVisitor)
    }
}

/// Parse a check YAML document and merge it with project `defaults` into a domain [`Check`].
/// `id` identifies the check (typically its file stem). Unknown fields are rejected.
pub fn load_check(text: &str, id: CheckId, defaults: &Defaults) -> Result<Check, ConfigError> {
    let dto: CheckFileDto =
        serde_norway::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    merge_check(dto, id, defaults)
}

fn merge_check(dto: CheckFileDto, id: CheckId, defaults: &Defaults) -> Result<Check, ConfigError> {
    let CheckFileDto {
        name: file_name,
        url: raw_url,
        viewport: vp,
        full_page,
        threshold: th,
        actions,
        spec,
        scenarios,
    } = dto;

    let name = file_name.unwrap_or_else(|| id.as_str().to_string());
    let base = defaults.base_url.as_deref();

    let raw_threshold = th.or(defaults.threshold).unwrap_or(DEFAULT_THRESHOLD);
    let threshold =
        Threshold::new(raw_threshold).ok_or(ConfigError::ThresholdRange(raw_threshold))?;

    let viewport = vp
        .or(defaults.viewport)
        .map(Viewport::from)
        .unwrap_or_default();
    if viewport.width == 0 || viewport.height == 0 {
        return Err(ConfigError::Invalid {
            name,
            reason: "viewport width and height must be > 0".to_string(),
        });
    }
    let url = resolve_url(&raw_url, base)?;

    let scenarios = match (spec, scenarios.is_empty()) {
        // single-spec sugar: top-level actions belong to the one scenario
        (Some(spec), true) => vec![to_scenario(name.clone(), actions, spec, base)?],
        (None, false) => {
            if !actions.is_empty() {
                return Err(ConfigError::Invalid {
                    name,
                    reason: "top-level `actions` cannot be combined with `scenarios`".into(),
                });
            }
            scenarios
                .into_iter()
                .enumerate()
                .map(|(i, s)| {
                    let sname = s.name.unwrap_or_else(|| format!("scenario {}", i + 1));
                    to_scenario(sname, s.actions, s.spec, base)
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        (Some(_), false) => {
            return Err(ConfigError::Invalid {
                name,
                reason: "specify either `spec` or `scenarios`, not both".into(),
            });
        }
        (None, true) => return Err(ConfigError::EmptySpec { name }),
    };

    Ok(Check {
        id,
        name,
        url,
        viewport,
        full_page,
        threshold,
        scenarios,
    })
}

fn to_scenario(
    name: String,
    actions: Vec<ActionDto>,
    spec: String,
    base: Option<&str>,
) -> Result<Scenario, ConfigError> {
    if spec.trim().is_empty() {
        return Err(ConfigError::EmptySpec { name });
    }
    let actions = actions
        .into_iter()
        .map(|a| to_action(a, base))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Scenario {
        name,
        actions,
        spec,
    })
}

fn to_action(dto: ActionDto, base: Option<&str>) -> Result<Action, ConfigError> {
    Ok(match dto {
        ActionDto::Goto(a) => Action::Goto {
            url: resolve_url(&a.url, base)?,
        },
        ActionDto::Click(a) => Action::Click { target: a.target },
        ActionDto::Fill(a) => Action::Fill {
            target: a.target,
            value: a.value,
        },
        ActionDto::WaitFor(a) => Action::WaitFor { target: a.target },
    })
}

/// Resolve a possibly-relative URL against `base`. Absolute URLs pass through (normalized);
/// relative ones require a `base_url`. Malformed or unresolvable URLs are rejected.
fn resolve_url(raw: &str, base: Option<&str>) -> Result<TargetUrl, ConfigError> {
    if let Ok(abs) = Url::parse(raw) {
        return Ok(TargetUrl::new(abs.to_string()));
    }
    let base_str = base.ok_or_else(|| ConfigError::InvalidUrl {
        url: raw.to_string(),
    })?;
    let base_url = Url::parse(base_str).map_err(|_| ConfigError::InvalidUrl {
        url: base_str.to_string(),
    })?;
    let joined = base_url.join(raw).map_err(|_| ConfigError::InvalidUrl {
        url: raw.to_string(),
    })?;
    Ok(TargetUrl::new(joined.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults_with_base() -> Defaults {
        Defaults {
            base_url: Some("https://example.com".to_string()),
            viewport: Some(ViewportDto {
                width: 1024,
                height: 768,
            }),
            threshold: Some(0.6),
            model: None,
        }
    }

    #[test]
    fn project_config_should_parse_defaults_and_suites() {
        let yaml = r#"
defaults:
  base_url: https://example.com
  threshold: 0.8
  model: claude-sonnet-5
concurrency: 8
suites:
  smoke:
    - checks/login.yaml
    - checks/top.yaml
"#;
        let cfg = ProjectConfig::from_yaml(yaml).expect("valid config");
        assert_eq!(cfg.concurrency, 8);
        assert_eq!(
            cfg.defaults.base_url.as_deref(),
            Some("https://example.com")
        );
        assert_eq!(cfg.suites["smoke"].len(), 2);
    }

    #[test]
    fn project_config_should_default_concurrency_and_retry() {
        let cfg = ProjectConfig::from_yaml("defaults: {}").expect("valid");
        assert_eq!(cfg.concurrency, 4);
        assert_eq!(cfg.retry.on_error, 1);
    }

    #[test]
    fn project_config_should_reject_unknown_field() {
        let err = ProjectConfig::from_yaml("concurency: 4").expect_err("typo rejected");
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn project_config_should_reject_zero_concurrency() {
        let err = ProjectConfig::from_yaml("concurrency: 0").expect_err("zero rejected");
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn load_check_should_reject_zero_viewport() {
        let yaml = "url: https://x.test/\nviewport: { width: 0, height: 0 }\nspec: ok\n";
        let err = load_check(yaml, CheckId::new("c"), &Defaults::default()).expect_err("zero vp");
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn load_check_should_apply_defaults_when_omitted() {
        let yaml = "url: /login\nspec: submit is visible\n";
        let check = load_check(yaml, CheckId::new("login"), &defaults_with_base()).expect("ok");
        assert_eq!(check.threshold.get(), 0.6); // from defaults
        assert_eq!(
            check.viewport,
            Viewport {
                width: 1024,
                height: 768
            }
        );
    }

    #[test]
    fn load_check_should_override_defaults() {
        let yaml = "url: /login\nthreshold: 0.9\nspec: submit is visible\n";
        let check = load_check(yaml, CheckId::new("login"), &defaults_with_base()).expect("ok");
        assert_eq!(check.threshold.get(), 0.9);
    }

    #[test]
    fn load_check_should_fall_back_to_builtin_threshold() {
        let yaml = "url: https://x.test/\nspec: ok\n";
        let check = load_check(yaml, CheckId::new("c"), &Defaults::default()).expect("ok");
        assert_eq!(check.threshold.get(), 0.7);
    }

    #[test]
    fn load_check_should_resolve_relative_url_against_base_url() {
        let yaml = "url: /orders\nspec: table shown\n";
        let check = load_check(yaml, CheckId::new("o"), &defaults_with_base()).expect("ok");
        assert_eq!(check.url.as_str(), "https://example.com/orders");
    }

    #[test]
    fn load_check_should_keep_absolute_url() {
        let yaml = "url: https://other.test/x\nspec: ok\n";
        let check = load_check(yaml, CheckId::new("o"), &defaults_with_base()).expect("ok");
        assert_eq!(check.url.as_str(), "https://other.test/x");
    }

    #[test]
    fn load_check_should_reject_relative_url_without_base() {
        let yaml = "url: /orders\nspec: ok\n";
        let err = load_check(yaml, CheckId::new("o"), &Defaults::default()).expect_err("no base");
        assert!(matches!(err, ConfigError::InvalidUrl { .. }));
    }

    #[test]
    fn load_check_should_reject_empty_spec() {
        let yaml = "url: https://x.test/\nspec: \"   \"\n";
        let err = load_check(yaml, CheckId::new("c"), &Defaults::default()).expect_err("empty");
        assert!(matches!(err, ConfigError::EmptySpec { .. }));
    }

    #[test]
    fn load_check_should_reject_threshold_out_of_range() {
        let yaml = "url: https://x.test/\nthreshold: 1.5\nspec: ok\n";
        let err = load_check(yaml, CheckId::new("c"), &Defaults::default()).expect_err("range");
        assert!(matches!(err, ConfigError::ThresholdRange(t) if t == 1.5));
    }

    #[test]
    fn load_check_should_reject_unknown_field() {
        let yaml = "url: https://x.test/\ntheshold: 0.7\nspec: ok\n";
        let err = load_check(yaml, CheckId::new("c"), &Defaults::default()).expect_err("typo");
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn load_check_should_reject_unknown_action() {
        let yaml = "url: https://x.test/\nactions:\n  - scroll: { target: x }\nspec: ok\n";
        let err = load_check(yaml, CheckId::new("c"), &Defaults::default()).expect_err("unknown");
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn load_check_should_reject_unknown_field_in_action() {
        let yaml = "url: https://x.test/\nactions:\n  - fill: { target: x, value: y, oops: 1 }\nspec: ok\n";
        let err = load_check(yaml, CheckId::new("c"), &Defaults::default()).expect_err("extra");
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn load_check_should_parse_actions() {
        let yaml = r#"
url: https://x.test/login
actions:
  - fill: { target: email, value: a@b.com }
  - wait_for: { target: "input[name=password]" }
spec: form is shown
"#;
        let check = load_check(yaml, CheckId::new("c"), &Defaults::default()).expect("ok");
        let actions = &check.scenarios[0].actions;
        assert_eq!(actions.len(), 2);
        assert!(matches!(&actions[0], Action::Fill { target, value }
            if target == "email" && value == "a@b.com"));
        assert!(matches!(&actions[1], Action::WaitFor { target }
            if target == "input[name=password]"));
    }

    #[test]
    fn load_check_should_parse_multiple_scenarios() {
        let yaml = r#"
url: https://x.test/orders
scenarios:
  - name: has orders
    spec: table is shown
  - name: server error
    spec: retry button is shown
"#;
        let check = load_check(yaml, CheckId::new("orders"), &Defaults::default()).expect("ok");
        assert_eq!(check.scenarios.len(), 2);
        assert_eq!(check.scenarios[1].name, "server error");
    }

    #[test]
    fn load_check_should_reject_both_spec_and_scenarios() {
        let yaml = r#"
url: https://x.test/o
spec: top spec
scenarios:
  - spec: inner spec
"#;
        let err = load_check(yaml, CheckId::new("o"), &Defaults::default()).expect_err("both");
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn load_check_should_reject_scenario_with_mock_until_m3() {
        // `mock` is an M3 precondition, not yet modeled: deny_unknown_fields rejects it.
        let yaml = r#"
url: https://x.test/o
scenarios:
  - name: server error
    mock: [{ url: "**/api", status: 500 }]
    spec: error shown
"#;
        let err = load_check(yaml, CheckId::new("o"), &Defaults::default()).expect_err("mock");
        assert!(matches!(err, ConfigError::Parse(_)));
    }
}

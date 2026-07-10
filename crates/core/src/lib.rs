//! `sentinel-core` — domain types, port traits, and judgment orchestration for Sentinel.
//!
//! This crate defines the domain model (`Check`, `Scenario`, `Verdict`, `Evidence`,
//! `Judgment`, `CheckResult`, …) and the port traits (`Browser`, `Judge`, `Store`,
//! `Reporter`). Adapters (`sentinel-browser`, `sentinel-ai`, `sentinel-store`) depend on
//! this crate and implement its ports; `core` stays free of adapter-specific dependencies.
//!
//! Port traits, config, and orchestration arrive in later M0 tasks (T-M0-08 onward).

mod domain;
mod error;

pub use domain::{
    Action, Check, CheckId, CheckResult, Confidence, Evidence, Judgment, Scenario, TargetUrl,
    Threshold, Verdict, Viewport, Violation,
};
pub use error::{AiError, BrowserError, ConfigError, RunError, StoreError};

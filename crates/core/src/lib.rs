//! `sentinel-core` — domain types, port traits, and judgment orchestration for Sentinel.
//!
//! This crate defines the domain model (`Check`, `Scenario`, `Verdict`, `Evidence`,
//! `Judgment`, `CheckResult`, …) and the port traits (`Browser`, `Judge`, `Store`,
//! `Reporter`). Adapters (`sentinel-browser`, `sentinel-ai`, `sentinel-store`) depend on
//! this crate and implement its ports; `core` stays free of adapter-specific dependencies.
//!
//! Judgment orchestration (driving the ports) arrives with the M1 walking skeleton.

mod config;
mod domain;
mod error;
mod ports;

pub use config::{Defaults, ProjectConfig, Retry, ViewportDto, load_check};
pub use domain::{
    Action, Check, CheckId, CheckResult, Confidence, Evidence, Judgment, Scenario, TargetUrl,
    Threshold, Verdict, Viewport, Violation,
};
pub use error::{AiError, BrowserError, ConfigError, RunError, StoreError};
pub use ports::{Browser, Judge, Reporter, Store};

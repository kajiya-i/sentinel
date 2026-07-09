//! `sentinel-ai` — `Judge` port implementation (reqwest + Claude, hand-rolled structs).
//!
//! Builds the structured judgment request (evidence + spec, json_schema-constrained), calls
//! Claude, and maps the response to a domain `Judgment` (verdict / confidence / violations),
//! including model escalation and error mapping. Depends on `sentinel-core` and implements its
//! `Judge` port.
//!
//! Skeleton: the adapter is implemented in M1 and M4.

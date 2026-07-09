//! `sentinel-browser` — `Browser` port implementation over chromiumoxide (CDP).
//!
//! Opens pages, arranges conditions (CDP Fetch interception, cookie/session), waits for the
//! page to settle, and collects evidence (screenshot + pruned accessibility tree). Depends on
//! `sentinel-core` and implements its `Browser` port.
//!
//! Skeleton: the adapter is implemented in M1–M3.

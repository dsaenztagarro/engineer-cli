//! The offline write queue — the durable, ordered log of deferred mutations
//! (the write half of offline-tolerance; `docs/designs/briefs/proposed/offline-write.brief.md`,
//! EPIC #98). The read half is `crate::timer_cache`; this module is its writing
//! sibling: intents enqueue when the wire is down, replay in order when it
//! returns, and surface loudly when the server disagrees. The queue holds
//! pending intents only, until they sync — the server stays authoritative,
//! never this file.
#![allow(dead_code)]

mod intent;
mod store;

#[allow(unused_imports)]
pub use intent::{Intent, IntentKind, IntentState};
#[allow(unused_imports)]
pub use store::{QueueDocView, QueueError, QueueStore, QueueSummary, ReplayGuard};

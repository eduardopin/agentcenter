//! Composite handoff-pickup: types only, consumed by CC5/CC6/CC7. No
//! implementation here; this card neither clears nor starts sessions.
//!
//! Stages advance strictly in order. A failed or unknown pickup reports the
//! last stage it reached and the old session, so the owner can resume by hand.
use super::{Artifact, State};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    HandoffRunning,
    /// Handoff evidence accepted (artifact verified), nothing cleared yet.
    HandoffPersisted,
    Clearing,
    NewSessionConfirmed,
    PickupSent,
    PickupLoaded,
}

/// Where to resume manually: the old session's ID and transcript file.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OldSession {
    pub session_id: String,
    pub session_file: PathBuf,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Progress {
    pub operation_id: String,
    pub state: State,
    /// Last stage reached; on failure/unknown, where it stopped.
    pub stage: Stage,
    pub code: &'static str,
    pub old_session: OldSession,
    /// Present from [`Stage::HandoffPersisted`] on.
    pub handoff: Option<Artifact>,
    pub new_session_id: Option<String>,
}

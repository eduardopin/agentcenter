//! Claude Code adapter: Handoff and Compact typed into the session's Herdr pane
//! (`agent.prompt`, protocol 22), completed only from evidence, never from the
//! send.
//!
//! Capability is enabled only when all hold: the agent is `claude`; the session
//! maps to a Herdr pane with explicit workspace/tab/pane; `/proc` starttime,
//! `pane.process_info` and `agent.get` agree on pid and `agent_session_id`; the
//! agent is `idle`; the transcript resolves; no squad-vigia swap claim exists
//! for the session (`~/.claude/squad/trocas/<session>.claim`, stat only; only
//! "not found" counts as no claim). Any failure is a closed code in
//! `disabled_reason`.
//!
//! The text sent is closed here: Compact is `/compact`; Handoff is
//! `/clear-pickup handoff agentcenter-op:<operation_id>`. Everything is
//! revalidated by [`herdr::Endpoint`] immediately before the prompt. No queue:
//! busy or another operation in flight for the session rejects.
//!
//! Evidence is read from the transcript only on demand (start/status), from
//! the byte offset the transcript had just before the send, never by the
//! sampler. Compact completes on a `system`/`compact_boundary` entry with
//! `compactMetadata.trigger == "manual"` of the same `sessionId`, timestamped
//! at or after both the send and the `/compact` user entry (Claude writes that
//! user entry after the boundary in file order, so order is by timestamp).
//! Handoff completes on a user entry carrying the operation mark, then a
//! `Write` tool call whose result succeeded for a path under
//! `<git-root>/.claude/handoffs/`, `~/.claude/handoffs/` or
//! `~/.claude/squad/handoffs/`, with the file present, regular, reached without
//! any symlink, owned by the UID and modified at or after the send. Its size
//! and sha256 are recorded; its bytes are never kept or shown.
//!
//! Without evidence by `evidence_timeout` the operation is `unknown` and holds
//! its session for `hold`, counted from when it became `unknown`; a later
//! status with the same operation ID re-reads the evidence of that same
//! operation and session only.
//!
//! Handoff-pickup (CC6) is its own entry point, [`Controller::pickup_start`] /
//! [`Controller::pickup_status`], returning [`pickup::Progress`]; it is not an
//! [`Action`]. Sequence: the Handoff above until `persisted`; a private 0400
//! copy of exactly the acknowledged bytes (sha256 of the ack) in the
//! AgentCenter handoffs directory; wait for `idle`; `/clear` after full
//! revalidation; a different `agent_session_id` in the same pane for the same
//! pid+starttime; `/clear-pickup pickup <copy>` to the new session; loaded
//! when the new transcript shows that prompt and then a successful `Read` of
//! the copy. Each stage has a deadline; a timeout is `unknown` at that stage,
//! and nothing is retried or sent after `unknown`. Nothing deletes a
//! transcript or a handoff: after `/clear` the old transcript stays on disk and
//! [`resume_command`] names the old session.
use super::{
    Action, Artifact, Capability, Evidence, Outcome, State, Telemetry, herdr, hex,
    operation_id_valid,
    pickup::{OldSession, Progress, Stage},
    sha256_reader,
};
use crate::overlay::transcript::{claude::find, parse_iso_ms};
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const COMPACT_TEXT: &str = "/compact";
pub const HANDOFF_TEXT: &str = "/clear-pickup handoff";
pub const MARK: &str = "agentcenter-op:";
pub const CLEAR_TEXT: &str = "/clear";
pub const PICKUP_TEXT: &str = "/clear-pickup pickup";
/// Transcript bytes read per evaluation, and largest line considered.
const SCAN_LIMIT: u64 = 256 << 20;
const LINE_LIMIT: u64 = 16 << 20;
/// Largest handoff file hashed.
const ARTIFACT_LIMIT: u64 = 16 << 20;
const MAX_OPERATIONS: usize = 256;
const TELEMETRY: usize = 128;

/// Closed codes: disabled reasons and operation results.
pub mod code {
    pub const NOT_CLAUDE: &str = "not_claude";
    pub const NO_HERDR_PANE: &str = "no_herdr_pane";
    pub const NO_SESSION_ID: &str = "no_session_id";
    pub const NO_TRANSCRIPT: &str = "no_transcript";
    pub const VIGIA_SWAP: &str = "vigia_swap";
    /// The claim path could not be stat'ed (other than absent): fail closed.
    pub const CLAIM_UNREADABLE: &str = "claim_unreadable";
    pub const IDENTITY: &str = "identity";
    pub const STALE: &str = "stale";
    pub const AMBIGUOUS: &str = "ambiguous";
    pub const BUSY: &str = "busy";
    pub const SESSION_MISMATCH: &str = "session_mismatch";
    pub const UNSAFE_SOCKET: &str = "unsafe_socket";
    pub const TRANSPORT: &str = "transport";
    pub const INVALID: &str = "invalid";
    pub const INVALID_REQUEST: &str = "invalid_request";
    pub const DUPLICATE_MISMATCH: &str = "duplicate_mismatch";
    pub const PENDING: &str = "pending";
    pub const CAPACITY: &str = "capacity";
    pub const SENT: &str = "sent";
    pub const UNKNOWN_EFFECT: &str = "unknown_effect";
    pub const TIMEOUT: &str = "timeout";
    pub const TRANSCRIPT_REWRITTEN: &str = "transcript_rewritten";
    pub const TRANSCRIPT_UNREADABLE: &str = "transcript_unreadable";
    pub const COMPACTED: &str = "compacted";
    pub const PERSISTED: &str = "persisted";
    pub const UNKNOWN_OPERATION: &str = "unknown_operation";
    // Handoff-pickup (CC6). Running codes name the stage just reached.
    /// Private copy written and verified; waiting for `idle` to `/clear`.
    pub const COPIED: &str = "copied";
    pub const CLEAR_SENT: &str = "clear_sent";
    pub const NEW_SESSION: &str = "new_session";
    pub const PICKUP_SENT: &str = "pickup_sent";
    pub const PICKUP_LOADED: &str = "pickup_loaded";
    /// The handoff or its copy no longer matches the acknowledged sha256.
    pub const COPY_MISMATCH: &str = "copy_mismatch";
    /// The copy could not be written (directory unsafe, I/O).
    pub const COPY_FAILED: &str = "copy_failed";
    /// `/clear` did not change the session by the deadline; one Escape sent.
    pub const NOT_CLEARED: &str = "not_cleared";
    /// As [`NOT_CLEARED`], but the agent was working: no Escape.
    pub const NOT_CLEARED_WORKING: &str = "not_cleared_working";
    /// As [`NOT_CLEARED`], but the Escape was refused or not confirmed.
    pub const ESCAPE_FAILED: &str = "escape_failed";
    /// The copy's path cannot travel as one `/clear-pickup` argument
    /// (whitespace, `<`, `>`, control or non-UTF-8): refused before `/clear`.
    pub const UNSAFE_COPY_PATH: &str = "unsafe_copy_path";
}

fn herdr_code(e: herdr::Error) -> &'static str {
    match e {
        herdr::Error::Invalid => code::INVALID,
        herdr::Error::Identity => code::IDENTITY,
        herdr::Error::Stale => code::STALE,
        herdr::Error::Ambiguous => code::AMBIGUOUS,
        herdr::Error::Busy => code::BUSY,
        herdr::Error::Session => code::SESSION_MISMATCH,
        herdr::Error::UnsafeSocket => code::UNSAFE_SOCKET,
        herdr::Error::Transport => code::TRANSPORT,
        herdr::Error::Unknown => code::UNKNOWN_EFFECT,
    }
}

/// The monitored session as the adapter needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub target: herdr::Target,
    pub session_id: String,
    pub cwd: Option<String>,
}
impl Subject {
    pub fn from_session(s: &crate::session::Session) -> Result<Self, &'static str> {
        if s.agent != "claude" {
            return Err(code::NOT_CLAUDE);
        }
        let target = herdr::Target::from_session(s).ok_or(code::NO_HERDR_PANE)?;
        if [&target.workspace_id, &target.tab_id, &target.pane_id]
            .iter()
            .any(|v| v.is_empty() || matches!(v.as_str(), "current" | "default"))
        {
            return Err(code::NO_HERDR_PANE);
        }
        let session_id = s
            .overlay
            .as_ref()
            .and_then(|o| o.agent_session_id.clone())
            .ok_or(code::NO_SESSION_ID)?;
        Ok(Self {
            target,
            session_id,
            cwd: s.cwd.clone(),
        })
    }
}

/// Filesystem roots of the user running the monitor.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: PathBuf,
    /// Expected owner of handoff files; [`super::pi::getuid`] in production.
    pub uid: u32,
    /// AgentCenter handoffs directory, where pickup copies go:
    /// `<state>/agentcenter/handoffs`, `<state>` as in [`Env::with_state_home`].
    pub handoffs: PathBuf,
}
impl Env {
    /// Roots of `home`, `XDG_STATE_HOME` read from the process, as the
    /// cockpit's handoff index does ([`crate::overlay::transcript::Transcripts::from_env`]).
    pub fn new(home: PathBuf) -> Self {
        Self::with_state_home(home, std::env::var_os("XDG_STATE_HOME"))
    }
    /// As [`Env::new`] with `XDG_STATE_HOME` given: `<state_home>` when
    /// absolute (so never empty), else `<home>/.local/state`.
    pub fn with_state_home(home: PathBuf, state_home: Option<OsString>) -> Self {
        let state = state_home
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".local/state"));
        Self {
            handoffs: state.join("agentcenter/handoffs"),
            home,
            uid: super::pi::getuid(),
        }
    }
    fn claim(&self, session_id: &str) -> PathBuf {
        self.home
            .join(".claude/squad/trocas")
            .join(format!("{session_id}.claim"))
    }
    /// Only an absent claim allows a send; any other stat error fails closed.
    fn no_claim(&self, session_id: &str) -> Result<(), &'static str> {
        match fs::symlink_metadata(self.claim(session_id)) {
            Ok(_) => Err(code::VIGIA_SWAP),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(code::CLAIM_UNREADABLE),
        }
    }
    fn roots(&self, cwd: Option<&str>) -> Vec<PathBuf> {
        let mut roots = vec![
            self.home.join(".claude/handoffs"),
            self.home.join(".claude/squad/handoffs"),
        ];
        if let Some(root) = cwd.and_then(git_root) {
            roots.push(root.join(".claude/handoffs"));
        }
        roots
    }
}

/// Nearest ancestor of `cwd` holding `.git` (directory or worktree file).
fn git_root(cwd: &str) -> Option<PathBuf> {
    let cwd = Path::new(cwd);
    if !cwd.is_absolute() {
        return None;
    }
    cwd.ancestors()
        .find(|p| fs::symlink_metadata(p.join(".git")).is_ok())
        .map(Path::to_path_buf)
}

/// Claude session IDs are lowercase UUIDs; anything else never becomes a path.
fn session_id_valid(s: &str) -> bool {
    operation_id_valid(s)
}

#[derive(Debug, Clone)]
pub struct Capabilities {
    pub handoff: Capability,
    pub compact: Capability,
}

#[derive(Debug)]
struct Operation {
    action: Action,
    subject: Subject,
    transcript: Option<PathBuf>,
    roots: Vec<PathBuf>,
    offset: u64,
    sent_ms: u64,
    sent_at: Option<Instant>,
    /// When the operation last became `unknown`; `hold` counts from here.
    unknown_at: Option<Instant>,
    state: State,
    code: &'static str,
    evidence: Option<Evidence>,
}

pub struct Controller {
    endpoint: herdr::Endpoint,
    env: Env,
    /// Herdr deadline for all checks plus the prompt, at most 5 s.
    pub io_timeout: Duration,
    /// After the send, how long evidence may take before `unknown`.
    pub evidence_timeout: Duration,
    /// How long an `unknown` operation still blocks new ones on its session,
    /// counted from when it became `unknown` (not from the send).
    pub hold: Duration,
    ops: HashMap<String, Operation>,
    telemetry: VecDeque<Telemetry>,
    /// Pickup: after the copy, how long to wait for `idle` before `/clear`.
    pub idle_timeout: Duration,
    /// Pickup: after `/clear`, how long the pane may keep the old session.
    pub clear_timeout: Duration,
    /// Pickup: from the new session, how long until the copy is read.
    pub load_timeout: Duration,
    pickups: HashMap<String, Pickup>,
}

#[derive(Debug)]
struct Pickup {
    subject: Subject,
    old_file: PathBuf,
    stage: Stage,
    state: State,
    code: &'static str,
    /// The private copy, from [`Stage::HandoffPersisted`] on.
    copy: Option<Artifact>,
    new_session: Option<String>,
    /// When the current stage began: its deadline counts from here.
    stage_at: Instant,
    unknown_at: Option<Instant>,
    /// Wall clock just before the pickup prompt; earlier entries never count.
    pickup_ms: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Controller {
    pub fn new(endpoint: herdr::Endpoint, env: Env) -> Self {
        Self {
            endpoint,
            env,
            io_timeout: Duration::from_secs(3),
            evidence_timeout: Duration::from_secs(600),
            hold: Duration::from_secs(600),
            ops: HashMap::new(),
            telemetry: VecDeque::new(),
            idle_timeout: Duration::from_secs(60),
            clear_timeout: Duration::from_secs(15),
            load_timeout: Duration::from_secs(180),
            pickups: HashMap::new(),
        }
    }

    pub fn telemetry(&self) -> impl Iterator<Item = &Telemetry> {
        self.telemetry.iter()
    }

    fn record(&mut self, action: Action, code: &'static str, since: Instant, id: &str) {
        if self.telemetry.len() == TELEMETRY {
            self.telemetry.pop_front();
        }
        let ms = since.elapsed().as_millis() as u64;
        self.telemetry
            .push_back(Telemetry::new(action, code, ms, id));
    }

    fn in_flight(&self, session_id: &str) -> bool {
        let held = |state: State, unknown_at: Option<Instant>| match state {
            State::Accepted | State::Running => true,
            State::Unknown => unknown_at.is_none_or(|t| t.elapsed() < self.hold),
            _ => false,
        };
        self.ops
            .values()
            .any(|o| o.subject.session_id == session_id && held(o.state, o.unknown_at))
            || self.pickups.values().any(|p| {
                (p.subject.session_id == session_id || p.new_session.as_deref() == Some(session_id))
                    && held(p.state, p.unknown_at)
            })
    }

    /// Local checks then a read-only Herdr probe. Returns the transcript.
    fn probe(&self, s: &Subject) -> Result<PathBuf, &'static str> {
        if !session_id_valid(&s.session_id) {
            return Err(code::NO_SESSION_ID);
        }
        if s.target.adapter != "claude" {
            return Err(code::NOT_CLAUDE);
        }
        self.env.no_claim(&s.session_id)?;
        let transcript = find(
            &self.env.home.join(".claude/projects"),
            s.cwd.as_deref(),
            &s.session_id,
        )
        .ok_or(code::NO_TRANSCRIPT)?;
        self.endpoint
            .check_agent(&s.target, &s.session_id, self.io_timeout)
            .map_err(herdr_code)?;
        Ok(transcript)
    }

    /// Same answer for both actions: the preconditions do not differ.
    pub fn capabilities(&self, s: &Subject) -> Capabilities {
        let c = if self.in_flight(&s.session_id) {
            Capability::disabled(code::PENDING)
        } else {
            match self.probe(s) {
                Ok(_) => Capability::enabled(),
                Err(c) => Capability::disabled(c),
            }
        };
        Capabilities {
            handoff: c.clone(),
            compact: c,
        }
    }

    fn outcome(id: &str, o: &Operation) -> Outcome {
        Outcome {
            operation_id: id.to_string(),
            action: o.action,
            state: o.state,
            code: o.code,
            evidence: o.evidence.clone(),
        }
    }

    /// Sends at most once per operation ID. A repeated ID with the same action
    /// and session returns the current status (re-evaluated), never resends.
    pub fn start(&mut self, operation_id: &str, action: Action, s: &Subject) -> Outcome {
        let started = Instant::now();
        let reject = |code| Outcome {
            operation_id: operation_id.to_string(),
            action,
            state: State::Rejected,
            code,
            evidence: None,
        };
        if !operation_id_valid(operation_id) {
            return reject(code::INVALID_REQUEST);
        }
        if let Some(o) = self.ops.get(operation_id) {
            if o.action == action && o.subject == *s {
                return self.status(operation_id).unwrap_or_else(reject);
            }
            return reject(code::DUPLICATE_MISMATCH);
        }
        if self.in_flight(&s.session_id) {
            self.record(action, code::PENDING, started, operation_id);
            return reject(code::PENDING);
        }
        if self.ops.len() >= MAX_OPERATIONS {
            self.ops.retain(|_, o| !o.state.is_terminal());
            if self.ops.len() >= MAX_OPERATIONS {
                return reject(code::CAPACITY);
            }
        }
        let mut op = Operation {
            action,
            subject: s.clone(),
            transcript: None,
            roots: self.env.roots(s.cwd.as_deref()),
            offset: 0,
            sent_ms: 0,
            sent_at: None,
            unknown_at: None,
            state: State::Rejected,
            code: code::INVALID,
            evidence: None,
        };
        let sent = self.probe(s).and_then(|transcript| {
            op.offset = fs::metadata(&transcript)
                .map_err(|_| code::NO_TRANSCRIPT)?
                .len();
            op.transcript = Some(transcript);
            let text = match action {
                Action::Compact => COMPACT_TEXT.to_string(),
                Action::Handoff => format!("{HANDOFF_TEXT} {MARK}{operation_id}"),
            };
            // A swap may have been claimed while Herdr was probed.
            self.env.no_claim(&s.session_id)?;
            op.sent_ms = now_ms();
            let r = self
                .endpoint
                .prompt(&s.target, &s.session_id, &text, self.io_timeout);
            if r == Err(herdr::Error::Unknown) {
                op.sent_at = Some(Instant::now());
            }
            r.map_err(herdr_code)
        });
        match sent {
            Ok(()) => {
                op.sent_at = Some(Instant::now());
                (op.state, op.code) = (State::Running, code::SENT);
            }
            Err(code::UNKNOWN_EFFECT) => {
                (op.state, op.code) = (State::Unknown, code::UNKNOWN_EFFECT);
                op.unknown_at = Some(Instant::now());
            }
            Err(c) => op.code = c,
        }
        self.record(action, op.code, started, operation_id);
        let id = operation_id.to_string();
        self.ops.insert(id.clone(), op);
        if self.ops[&id].sent_at.is_some() {
            return self.status(&id).unwrap_or_else(reject);
        }
        Self::outcome(&id, &self.ops[&id])
    }

    /// Re-reads the evidence of this operation; `unknown` may still complete.
    pub fn status(&mut self, operation_id: &str) -> Result<Outcome, &'static str> {
        let timeout = self.evidence_timeout;
        let o = self
            .ops
            .get_mut(operation_id)
            .ok_or(code::UNKNOWN_OPERATION)?;
        let (Some(sent_at), Some(transcript)) = (o.sent_at, o.transcript.clone()) else {
            return Ok(Self::outcome(operation_id, o));
        };
        if o.state.is_terminal() {
            return Ok(Self::outcome(operation_id, o));
        }
        let before = (o.state, o.code);
        match evidence(
            o.action,
            &transcript,
            o.offset,
            o.sent_ms,
            &o.subject.session_id,
            operation_id,
            &o.roots,
            self.env.uid,
        ) {
            Ok(Some(e)) => {
                o.state = State::Completed;
                o.code = match e {
                    Evidence::Compacted { .. } => code::COMPACTED,
                    Evidence::Persisted(_) => code::PERSISTED,
                };
                o.evidence = Some(e);
            }
            Ok(None) if sent_at.elapsed() >= timeout => {
                (o.state, o.code) = (State::Unknown, code::TIMEOUT)
            }
            Ok(None) => {}
            Err(c) => (o.state, o.code) = (State::Unknown, c),
        }
        if o.state == State::Unknown && (o.state, o.code) != before {
            o.unknown_at = Some(Instant::now());
        }
        let out = Self::outcome(operation_id, o);
        if (out.state, out.code) != before {
            let action = out.action;
            self.record(action, out.code, sent_at, operation_id);
        }
        Ok(out)
    }

    fn progress(id: &str, p: &Pickup) -> Progress {
        Progress {
            operation_id: id.to_string(),
            state: p.state,
            stage: p.stage,
            code: p.code,
            old_session: OldSession {
                session_id: p.subject.session_id.clone(),
                session_file: p.old_file.clone(),
            },
            handoff: p.copy.clone(),
            new_session_id: p.new_session.clone(),
        }
    }

    /// Starts a handoff-pickup: the Handoff of [`Controller::start`] under the
    /// same operation ID, then as many later stages as are ready now. A
    /// repeated ID for the same session returns the current progress and never
    /// resends; an ID already used by another operation is rejected.
    pub fn pickup_start(&mut self, operation_id: &str, s: &Subject) -> Progress {
        let reject = |code| Progress {
            operation_id: operation_id.to_string(),
            state: State::Rejected,
            stage: Stage::HandoffRunning,
            code,
            old_session: OldSession {
                session_id: s.session_id.clone(),
                session_file: PathBuf::new(),
            },
            handoff: None,
            new_session_id: None,
        };
        if !operation_id_valid(operation_id) {
            return reject(code::INVALID_REQUEST);
        }
        if let Some(p) = self.pickups.get(operation_id) {
            if p.subject == *s {
                return self.pickup_status(operation_id).unwrap_or_else(reject);
            }
            return reject(code::DUPLICATE_MISMATCH);
        }
        if self.ops.contains_key(operation_id) {
            return reject(code::DUPLICATE_MISMATCH);
        }
        if self.pickups.len() >= MAX_OPERATIONS {
            self.pickups.retain(|_, p| !p.state.is_terminal());
            if self.pickups.len() >= MAX_OPERATIONS {
                return reject(code::CAPACITY);
            }
        }
        let o = self.start(operation_id, Action::Handoff, s);
        let old_file = self
            .ops
            .get(operation_id)
            .and_then(|o| o.transcript.clone())
            .unwrap_or_default();
        let p = Pickup {
            subject: s.clone(),
            old_file,
            stage: Stage::HandoffRunning,
            // A handoff completed already still has its copy to make.
            state: match o.state {
                State::Completed => State::Running,
                s => s,
            },
            code: o.code,
            copy: None,
            new_session: None,
            stage_at: Instant::now(),
            unknown_at: (o.state == State::Unknown).then(Instant::now),
            pickup_ms: 0,
        };
        self.pickups.insert(operation_id.to_string(), p);
        self.pickup_status(operation_id).unwrap_or_else(reject)
    }

    /// Advances the pickup as far as it can go now (each Herdr call bounded by
    /// `io_timeout`, no sleeping) and returns where it is.
    pub fn pickup_status(&mut self, operation_id: &str) -> Result<Progress, &'static str> {
        if !self.pickups.contains_key(operation_id) {
            return Err(code::UNKNOWN_OPERATION);
        }
        while self.pickup_step(operation_id) {}
        Ok(Self::progress(operation_id, &self.pickups[operation_id]))
    }

    /// One stage transition attempt; `true` when it moved and may go on.
    fn pickup_step(&mut self, id: &str) -> bool {
        let p = &self.pickups[id];
        let (state, stage) = (p.state, p.stage);
        if state.is_terminal() {
            return false;
        }
        if state == State::Unknown {
            // Nothing is sent after unknown; only the pickup's own load
            // evidence may still reconcile it.
            if stage == Stage::PickupSent && self.loaded(id) == Ok(true) {
                self.pickup_set(
                    id,
                    State::Completed,
                    Stage::PickupLoaded,
                    code::PICKUP_LOADED,
                );
            }
            return false;
        }
        match stage {
            Stage::HandoffRunning => self.step_handoff(id),
            Stage::HandoffPersisted => self.step_clear(id),
            Stage::Clearing => self.step_clearing(id),
            Stage::NewSessionConfirmed => self.step_pickup(id),
            Stage::PickupSent => {
                let deadline = self.load_timeout;
                match self.loaded(id) {
                    Ok(true) => self.pickup_set(
                        id,
                        State::Completed,
                        Stage::PickupLoaded,
                        code::PICKUP_LOADED,
                    ),
                    _ if self.pickups[id].stage_at.elapsed() >= deadline => {
                        self.pickup_set(id, State::Unknown, stage, code::TIMEOUT)
                    }
                    _ => false,
                }
            }
            Stage::PickupLoaded => false,
        }
    }

    /// Sets state, stage and code; a stage change restarts its deadline and
    /// entering `unknown` starts the hold. Returns whether it may go on.
    fn pickup_set(&mut self, id: &str, state: State, stage: Stage, code: &'static str) -> bool {
        let p = self.pickups.get_mut(id).expect("pickup exists");
        if p.stage != stage {
            p.stage_at = Instant::now();
        }
        if state == State::Unknown && p.state != State::Unknown {
            p.unknown_at = Some(Instant::now());
        }
        (p.state, p.stage, p.code) = (state, stage, code);
        state == State::Running
    }

    fn step_handoff(&mut self, id: &str) -> bool {
        let Ok(o) = self.status(id) else {
            return self.pickup_set(id, State::Failed, Stage::HandoffRunning, code::INVALID);
        };
        match (o.state, o.evidence) {
            (State::Completed, Some(Evidence::Persisted(a))) => {
                let p = &self.pickups[id];
                match seal_copy(&a, &self.env, &p.subject.session_id, id) {
                    Ok(copy) => {
                        self.pickups.get_mut(id).expect("pickup exists").copy = Some(copy);
                        self.pickup_set(id, State::Running, Stage::HandoffPersisted, code::COPIED)
                    }
                    Err(c) => self.pickup_set(id, State::Failed, Stage::HandoffRunning, c),
                }
            }
            (State::Completed, _) => {
                self.pickup_set(id, State::Failed, Stage::HandoffRunning, code::INVALID)
            }
            (state, _) => {
                self.pickup_set(id, state, Stage::HandoffRunning, o.code);
                false
            }
        }
    }

    /// Claims (old and new session) and the copy, rechecked before each send.
    fn pickup_guard(&self, id: &str) -> Result<(), &'static str> {
        let p = &self.pickups[id];
        self.env.no_claim(&p.subject.session_id)?;
        if let Some(new) = &p.new_session {
            self.env.no_claim(new)?;
        }
        let copy = p.copy.as_ref().ok_or(code::COPY_MISMATCH)?;
        if !pickup_arg_safe(&copy.path) {
            return Err(code::UNSAFE_COPY_PATH);
        }
        if !copy_intact(copy, self.env.uid) {
            return Err(code::COPY_MISMATCH);
        }
        Ok(())
    }

    fn step_clear(&mut self, id: &str) -> bool {
        let stage = Stage::HandoffPersisted;
        if let Err(c) = self.pickup_guard(id) {
            return self.pickup_set(id, State::Failed, stage, c);
        }
        let p = &self.pickups[id];
        let r = self.endpoint.prompt(
            &p.subject.target,
            &p.subject.session_id,
            CLEAR_TEXT,
            self.io_timeout,
        );
        match r {
            Ok(()) => self.pickup_set(id, State::Running, Stage::Clearing, code::CLEAR_SENT),
            Err(herdr::Error::Busy) if p.stage_at.elapsed() >= self.idle_timeout => {
                self.pickup_set(id, State::Unknown, stage, code::TIMEOUT)
            }
            Err(herdr::Error::Busy) => false,
            Err(herdr::Error::Unknown) => {
                self.pickup_set(id, State::Unknown, Stage::Clearing, code::UNKNOWN_EFFECT)
            }
            Err(e) => self.pickup_set(id, State::Failed, stage, herdr_code(e)),
        }
    }

    fn step_clearing(&mut self, id: &str) -> bool {
        let stage = Stage::Clearing;
        let p = &self.pickups[id];
        let (t, old) = (&p.subject.target, p.subject.session_id.as_str());
        let late = p.stage_at.elapsed() >= self.clear_timeout;
        match self.endpoint.pane_session(t, self.io_timeout) {
            Ok((Some(new), _)) if new != old => {
                if !session_id_valid(&new) {
                    return self.pickup_set(id, State::Failed, stage, code::SESSION_MISMATCH);
                }
                self.pickups.get_mut(id).expect("pickup exists").new_session = Some(new);
                self.pickup_set(
                    id,
                    State::Running,
                    Stage::NewSessionConfirmed,
                    code::NEW_SESSION,
                )
            }
            Ok((Some(_), status)) if late => {
                // Still the old session: a dialog is likely open. One Escape,
                // never into a running turn nor into a session the vigia
                // claimed; the old session stays active.
                let c = if status.as_deref() == Some("working") {
                    code::NOT_CLEARED_WORKING
                } else if let Err(c) = self.env.no_claim(old) {
                    c
                } else {
                    match self.endpoint.escape(t, old, self.io_timeout) {
                        Ok(()) => code::NOT_CLEARED,
                        Err(herdr::Error::Busy) => code::NOT_CLEARED_WORKING,
                        Err(_) => code::ESCAPE_FAILED,
                    }
                };
                self.pickup_set(id, State::Failed, stage, c)
            }
            Ok(_) | Err(herdr::Error::Transport) if late => {
                self.pickup_set(id, State::Unknown, stage, code::TIMEOUT)
            }
            Ok(_) | Err(herdr::Error::Transport) => false,
            Err(e) => self.pickup_set(id, State::Failed, stage, herdr_code(e)),
        }
    }

    fn step_pickup(&mut self, id: &str) -> bool {
        let stage = Stage::NewSessionConfirmed;
        if let Err(c) = self.pickup_guard(id) {
            return self.pickup_set(id, State::Failed, stage, c);
        }
        let p = &self.pickups[id];
        let (Some(new), Some(copy)) = (p.new_session.clone(), p.copy.as_ref()) else {
            return self.pickup_set(id, State::Failed, stage, code::INVALID);
        };
        let Some(path) = copy.path.to_str() else {
            return self.pickup_set(id, State::Failed, stage, code::COPY_FAILED);
        };
        let text = format!("{PICKUP_TEXT} {path}");
        let sent_ms = now_ms();
        let r = self
            .endpoint
            .prompt(&p.subject.target, &new, &text, self.io_timeout);
        let late = p.stage_at.elapsed() >= self.load_timeout;
        if matches!(r, Ok(()) | Err(herdr::Error::Unknown)) {
            self.pickups.get_mut(id).expect("pickup exists").pickup_ms = sent_ms;
        }
        match r {
            Ok(()) => self.pickup_set(id, State::Running, Stage::PickupSent, code::PICKUP_SENT),
            Err(herdr::Error::Busy) if late => {
                self.pickup_set(id, State::Unknown, stage, code::TIMEOUT)
            }
            Err(herdr::Error::Busy) => false,
            Err(herdr::Error::Unknown) => {
                self.pickup_set(id, State::Unknown, Stage::PickupSent, code::UNKNOWN_EFFECT)
            }
            Err(e) => self.pickup_set(id, State::Failed, stage, herdr_code(e)),
        }
    }

    /// Whether the new session's transcript shows this pickup loaded.
    fn loaded(&self, id: &str) -> Result<bool, &'static str> {
        let p = &self.pickups[id];
        let (Some(new), Some(copy)) = (p.new_session.as_deref(), p.copy.as_ref()) else {
            return Ok(false);
        };
        let Some(transcript) = find(
            &self.env.home.join(".claude/projects"),
            p.subject.cwd.as_deref(),
            new,
        ) else {
            return Ok(false);
        };
        pickup_loaded(&transcript, p.pickup_ms, new, &copy.path)
    }
}

/// Text of a message: a string, or the `text` of its text blocks.
fn texts(message: &Value) -> Vec<&str> {
    match &message["content"] {
        Value::String(s) => vec![s.as_str()],
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect(),
        _ => Vec::new(),
    }
}

/// Calls `f` with each complete JSON entry of `session_id` timestamped at or
/// after `since_ms`, reading `transcript` from `offset` (at most
/// `SCAN_LIMIT` bytes; partial and oversized lines skipped).
fn entries(
    transcript: &Path,
    offset: u64,
    session_id: &str,
    since_ms: u64,
    mut f: impl FnMut(&Value, u64),
) -> Result<(), &'static str> {
    let mut file = File::open(transcript).map_err(|_| code::TRANSCRIPT_UNREADABLE)?;
    let len = file
        .metadata()
        .map_err(|_| code::TRANSCRIPT_UNREADABLE)?
        .len();
    if len < offset {
        return Err(code::TRANSCRIPT_REWRITTEN);
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| code::TRANSCRIPT_UNREADABLE)?;
    let mut reader = BufReader::new(file.take(SCAN_LIMIT));
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = (&mut reader)
            .take(LINE_LIMIT)
            .read_until(b'\n', &mut line)
            .map_err(|_| code::TRANSCRIPT_UNREADABLE)?;
        if n == 0 {
            return Ok(());
        }
        if line.last() != Some(&b'\n') {
            // Partial last line (still being written) or oversized: skip.
            if n as u64 >= LINE_LIMIT {
                let mut sink = Vec::new();
                let _ = reader.read_until(b'\n', &mut sink);
            }
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if v["sessionId"] != session_id {
            continue;
        }
        let Some(ts) = v["timestamp"].as_str().and_then(parse_iso_ms) else {
            continue;
        };
        if ts >= since_ms {
            f(&v, ts);
        }
    }
}

/// Evidence for one operation, reading the transcript from `offset`.
/// `Ok(None)`: not (yet) there. Nothing read is kept beyond the result.
#[allow(clippy::too_many_arguments)]
pub(crate) fn evidence(
    action: Action,
    transcript: &Path,
    offset: u64,
    sent_ms: u64,
    session_id: &str,
    operation_id: &str,
    roots: &[PathBuf],
    uid: u32,
) -> Result<Option<Evidence>, &'static str> {
    let mark = format!("{MARK}{operation_id}");
    let mut command_ms: Option<u64> = None;
    let mut boundaries = Vec::new();
    let mut writes: HashMap<String, String> = HashMap::new();
    let mut written = Vec::new();
    entries(transcript, offset, session_id, sent_ms, |v, ts| {
        match (action, v["type"].as_str()) {
            (Action::Compact, Some("user")) => {
                if texts(&v["message"])
                    .iter()
                    .any(|t| t.contains("<command-name>/compact</command-name>"))
                {
                    command_ms = Some(command_ms.map_or(ts, |c| c.min(ts)));
                }
            }
            (Action::Compact, Some("system")) => {
                if v["subtype"] == "compact_boundary" && v["compactMetadata"]["trigger"] == "manual"
                {
                    boundaries.push(ts);
                }
            }
            (Action::Handoff, Some("user")) => {
                if command_ms.is_none() {
                    if texts(&v["message"]).iter().any(|t| t.contains(&mark)) {
                        command_ms = Some(ts);
                    }
                    return;
                }
                let Some(blocks) = v["message"]["content"].as_array() else {
                    return;
                };
                for b in blocks {
                    if b["type"] != "tool_result" || b["is_error"] == true {
                        continue;
                    }
                    let Some(path) = b["tool_use_id"].as_str().and_then(|id| writes.get(id)) else {
                        continue;
                    };
                    if v["toolUseResult"]["filePath"].as_str() == Some(path.as_str()) {
                        written.push(path.clone());
                    }
                }
            }
            (Action::Handoff, Some("assistant")) if command_ms.is_some() => {
                let Some(blocks) = v["message"]["content"].as_array() else {
                    return;
                };
                for b in blocks {
                    if b["type"] == "tool_use"
                        && b["name"] == "Write"
                        && let (Some(id), Some(path)) =
                            (b["id"].as_str(), b["input"]["file_path"].as_str())
                    {
                        writes.insert(id.to_string(), path.to_string());
                    }
                }
            }
            _ => {}
        }
    })?;
    Ok(match action {
        Action::Compact => command_ms
            .and_then(|c| boundaries.into_iter().filter(|b| *b >= c).min())
            .map(|boundary_ms| Evidence::Compacted { boundary_ms }),
        Action::Handoff => written
            .iter()
            .find_map(|p| artifact(Path::new(p), roots, sent_ms, uid))
            .map(Evidence::Persisted),
    })
}

/// The handoff file, if it is under an allowed root, reached without any
/// symlink, a regular file owned by `uid`, modified at or after `sent_ms`.
pub(crate) fn artifact(path: &Path, roots: &[PathBuf], sent_ms: u64, uid: u32) -> Option<Artifact> {
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir)
        || !parts.all(|c| matches!(c, Component::Normal(_)))
        || !roots.iter().any(|r| path.starts_with(r) && path != r)
    {
        return None;
    }
    for p in path.ancestors() {
        if fs::symlink_metadata(p).ok()?.file_type().is_symlink() {
            return None;
        }
    }
    let before = fs::symlink_metadata(path).ok()?;
    if !before.is_file() {
        return None;
    }
    // O_NOFOLLOW | O_NONBLOCK: a swapped-in symlink or FIFO cannot redirect or block.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(0o400000 | 0o4000)
        .open(path)
        .ok()?;
    let m = file.metadata().ok()?;
    let modified_ms = (m.mtime().max(0) as u64)
        .saturating_mul(1000)
        .saturating_add(m.mtime_nsec().max(0) as u64 / 1_000_000);
    if !m.is_file()
        || m.dev() != before.dev()
        || m.ino() != before.ino()
        || m.uid() != uid
        || modified_ms < sent_ms
        || m.len() > ARTIFACT_LIMIT
    {
        return None;
    }
    let (digest, size) = sha256_reader(file.take(ARTIFACT_LIMIT + 1)).ok()?;
    if size != m.len() {
        return None;
    }
    Some(Artifact {
        path: path.to_path_buf(),
        size,
        sha256: hex(&digest),
        modified_ms,
    })
}

/// A regular file of `uid` reached without any symlink, opened without
/// following one, at most `ARTIFACT_LIMIT` bytes.
fn open_regular(path: &Path, uid: u32) -> Option<(File, fs::Metadata)> {
    if !path.is_absolute() {
        return None;
    }
    for p in path.ancestors() {
        if fs::symlink_metadata(p).ok()?.file_type().is_symlink() {
            return None;
        }
    }
    // O_NOFOLLOW | O_NONBLOCK, as in [`artifact`].
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(0o400000 | 0o4000)
        .open(path)
        .ok()?;
    let m = file.metadata().ok()?;
    (m.is_file() && m.uid() == uid && m.len() <= ARTIFACT_LIMIT).then_some((file, m))
}

/// Whether `copy` still has its recorded size and sha256 (read now).
pub(crate) fn copy_intact(copy: &Artifact, uid: u32) -> bool {
    let Some((file, m)) = open_regular(&copy.path, uid) else {
        return false;
    };
    matches!(sha256_reader(file.take(ARTIFACT_LIMIT + 1)),
        Ok((d, n)) if n == m.len() && n == copy.size && hex(&d) == copy.sha256)
}

/// Private read-only copy of the acknowledged handoff: the original is read
/// once, those very bytes are checked against the ack's size and sha256 and
/// written 0400 under `env.handoffs` (0700, no symlink on its path) as
/// `s<hex(session_id)>-<operation_id>.md`, never over an existing file; the
/// copy is then re-read and checked. The bytes are dropped on return.
pub(crate) fn seal_copy(
    ack: &Artifact,
    env: &Env,
    session_id: &str,
    operation_id: &str,
) -> Result<Artifact, &'static str> {
    if !operation_id_valid(operation_id) || !session_id_valid(session_id) {
        return Err(code::INVALID);
    }
    let (file, m) = open_regular(&ack.path, env.uid).ok_or(code::COPY_MISMATCH)?;
    let mut bytes = Vec::with_capacity(m.len() as usize);
    file.take(ARTIFACT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| code::COPY_MISMATCH)?;
    if bytes.len() as u64 != ack.size || hex(&super::sha256(&bytes)) != ack.sha256 {
        return Err(code::COPY_MISMATCH);
    }
    let dir = &env.handoffs;
    // A symlink above the directory would send the copy elsewhere.
    if dir
        .ancestors()
        .any(|p| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
    {
        return Err(code::COPY_FAILED);
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|_| code::COPY_FAILED)?;
    let d = fs::symlink_metadata(dir).map_err(|_| code::COPY_FAILED)?;
    if !d.is_dir() || d.uid() != env.uid {
        return Err(code::COPY_FAILED);
    }
    if d.mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| code::COPY_FAILED)?;
    }
    let name = format!("s{}-{operation_id}.md", hex(session_id.as_bytes()));
    let path = dir.join(&name);
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .custom_flags(0o400000)
        .open(&tmp)
        .map_err(|_| code::COPY_FAILED)?;
    let written = out.write_all(&bytes).and_then(|_| out.sync_all());
    drop(bytes);
    // A hard link never replaces an existing name; the temporary name is ours.
    let linked = written.and_then(|_| fs::hard_link(&tmp, &path));
    let _ = fs::remove_file(&tmp);
    linked.map_err(|_| code::COPY_FAILED)?;
    let (_, m) = open_regular(&path, env.uid).ok_or(code::COPY_MISMATCH)?;
    let modified_ms = (m.mtime().max(0) as u64)
        .saturating_mul(1000)
        .saturating_add(m.mtime_nsec().max(0) as u64 / 1_000_000);
    let copy = Artifact {
        path,
        size: ack.size,
        sha256: ack.sha256.clone(),
        modified_ms,
    };
    if !copy_intact(&copy, env.uid) {
        return Err(code::COPY_MISMATCH);
    }
    Ok(copy)
}

/// Whether `path` survives as the single argument of `/clear-pickup pickup`
/// and inside `<command-args>`: UTF-8, no whitespace, no `<`/`>`, no control
/// character (the evidence check splits on newline, `<` and `>`).
pub(crate) fn pickup_arg_safe(path: &Path) -> bool {
    path.to_str().is_some_and(|s| {
        !s.is_empty()
            && !s
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == '<' || c == '>')
    })
}

/// Pickup evidence in the new session's transcript: a user entry with the
/// `/clear-pickup` command whose argument is `pickup <copy>`, then a `Read`
/// of exactly `copy` whose result succeeded for that file. Entries of other
/// sessions, before `sent_ms`, or of another path never count.
pub(crate) fn pickup_loaded(
    transcript: &Path,
    sent_ms: u64,
    session_id: &str,
    copy: &Path,
) -> Result<bool, &'static str> {
    let Some(copy) = copy.to_str() else {
        return Ok(false);
    };
    let arg = format!("pickup {copy}");
    let mut prompted = false;
    let mut reads: Vec<String> = Vec::new();
    let mut loaded = false;
    entries(transcript, 0, session_id, sent_ms, |v, _| {
        if loaded {
            return;
        }
        let blocks = v["message"]["content"].as_array();
        match v["type"].as_str() {
            Some("user") if !prompted => {
                prompted = texts(&v["message"]).iter().any(|t| {
                    t.contains("/clear-pickup")
                        && t.split(['\n', '<', '>']).any(|part| part.trim() == arg)
                });
            }
            Some("assistant") if prompted => {
                for b in blocks.into_iter().flatten() {
                    if b["type"] == "tool_use"
                        && b["name"] == "Read"
                        && b["input"]["file_path"] == copy
                        && let Some(id) = b["id"].as_str()
                    {
                        reads.push(id.to_string());
                    }
                }
            }
            Some("user") => {
                loaded = blocks.into_iter().flatten().any(|b| {
                    b["type"] == "tool_result"
                        && b["is_error"] != true
                        && b["tool_use_id"]
                            .as_str()
                            .is_some_and(|id| reads.iter().any(|r| r == id))
                }) && v["toolUseResult"]["file"]["filePath"] == copy;
            }
            _ => {}
        }
    })?;
    Ok(loaded)
}

/// Manual resume of the session a pickup started from.
pub fn resume_command(p: &Progress) -> Option<String> {
    session_id_valid(&p.old_session.session_id)
        .then(|| format!("claude --resume {}", p.old_session.session_id))
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;

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
use super::{
    Action, Artifact, Capability, Evidence, Outcome, State, Telemetry, herdr, hex,
    operation_id_valid, sha256_reader,
};
use crate::overlay::transcript::{claude::find, parse_iso_ms};
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const COMPACT_TEXT: &str = "/compact";
pub const HANDOFF_TEXT: &str = "/clear-pickup handoff";
pub const MARK: &str = "agentcenter-op:";
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
}
impl Env {
    pub fn new(home: PathBuf) -> Self {
        Self {
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
        self.ops.values().any(|o| {
            o.subject.session_id == session_id
                && match o.state {
                    State::Accepted | State::Running => true,
                    State::Unknown => o.unknown_at.is_none_or(|t| t.elapsed() < self.hold),
                    _ => false,
                }
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
    let mark = format!("{MARK}{operation_id}");
    let mut command_ms: Option<u64> = None;
    let mut boundaries = Vec::new();
    let mut writes: HashMap<String, String> = HashMap::new();
    let mut written = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = (&mut reader)
            .take(LINE_LIMIT)
            .read_until(b'\n', &mut line)
            .map_err(|_| code::TRANSCRIPT_UNREADABLE)?;
        if n == 0 {
            break;
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
        if ts < sent_ms {
            continue;
        }
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
                    continue;
                }
                let Some(blocks) = v["message"]["content"].as_array() else {
                    continue;
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
                    continue;
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
    }
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

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;

//! Protocol 2 client. Node authenticates clients with kernel DAC (0700 directory,
//! 0600 socket) and nonce, not a nonexistent Node peercred getter. This client
//! additionally verifies the server's Linux SO_PEERCRED and /proc starttime.
//! Same-UID processes and root are not isolated by this protocol.
pub use super::{Capability, State};
use super::{herdr, pickup};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
const LIMIT: usize = 16 * 1024;
const NOFOLLOW: i32 = 0o400000;
const DIRECTORY: i32 = 0o200000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Nonce(String);
impl std::fmt::Debug for Nonce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Expected {
    pub manager_id: String,
    pub revision: String,
    pub session_id: String,
    pub session_file: String,
    pub leaf_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub handoff: Capability,
    pub compact: Capability,
    /// Protocol 3 replies only; never in the v2 manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_pickup: Option<PickupCapability>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PickupCapability {
    pub enabled: bool,
    pub disabled_reason: Option<String>,
    pub cancel_supported: bool,
    pub protocol_version: u32,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub handoff_directory: Option<PathBuf>,
    pub model_auth: Option<String>,
    pub protocol_version: u32,
    pub session_id: String,
    pub session_file: String,
    pub pid: u32,
    pub process_starttime: String,
    pub instance_nonce: Nonce,
    pub expected: Option<Expected>,
    pub socket: PathBuf,
    pub capabilities: Capabilities,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Handoff,
    Compact,
    Status,
    Cancel,
    /// Protocol 3: arms a pickup; only `/agentcenter-pickup` executes it.
    HandoffPickup,
}
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub protocol_version: u32,
    pub operation_id: String,
    pub action: Action,
    pub session_id: String,
    pub session_file: String,
    pub pid: u32,
    pub process_starttime: String,
    pub instance_nonce: Nonce,
    pub expected: Expected,
}
impl Endpoint {
    /// Retain this exact request for status/cancel; do not refresh its expected snapshot.
    pub fn request(&self, operation_id: String, action: Action) -> Result<Request, Error> {
        validate_endpoint(self)?;
        if !uuid(&operation_id) {
            return Err(Error::Invalid);
        }
        Ok(Request {
            protocol_version: 2,
            operation_id,
            action,
            session_id: self.session_id.clone(),
            session_file: self.session_file.clone(),
            pid: self.pid,
            process_starttime: self.process_starttime.clone(),
            instance_nonce: self.instance_nonce.clone(),
            expected: self.expected.clone().ok_or(Error::Invalid)?,
        })
    }
}
impl Request {
    pub fn status(&self) -> Self {
        let mut r = self.clone();
        r.action = Action::Status;
        r
    }
    pub fn cancel(&self) -> Self {
        let mut r = self.clone();
        r.action = Action::Cancel;
        r
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub handoff_directory: Option<PathBuf>,
    pub model_auth: Option<String>,
    pub expected: Option<Expected>,
    pub capabilities: Capabilities,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Compaction {
    pub capability: String,
    pub session_id: String,
    pub session_file: String,
    pub previous_leaf_id: Option<String>,
    pub entry_id: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub completed_at: Option<String>,
    pub cost_usd: Option<f64>,
    pub protocol_version: u32,
    pub operation_id: String,
    pub state: State,
    pub code: String,
    pub metadata: Option<Metadata>,
    pub artifact_path: Option<PathBuf>,
    pub expected: Option<Expected>,
    pub compaction: Option<Compaction>,
    /// Protocol 3 pickup record: paths, hashes, IDs and codes only.
    #[serde(default)]
    pub pickup: Option<PickupReply>,
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionRef {
    pub session_id: String,
    pub session_file: String,
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffRef {
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NewSessionRef {
    pub session_id: String,
    pub session_file: Option<String>,
    pub parent_session: Option<String>,
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PickupReply {
    pub stage: String,
    pub old_session: SessionRef,
    pub handoff: Option<HandoffRef>,
    pub new_session: Option<NewSessionRef>,
    pub loaded_entry_id: Option<String>,
}
#[derive(Debug)]
pub enum Error {
    Io(io::ErrorKind),
    Invalid,
    UnsafePath,
    Identity,
    Unknown,
    /// A protocol 2 extension (CC1) refused a protocol 3 request in its
    /// listener, before any dispatch: nothing ran. Not ambiguous.
    UnsupportedProtocol,
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e.kind())
    }
}
fn invalid() -> Error {
    Error::Invalid
}
fn decimal(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) && (s == "0" || !s.starts_with('0'))
}
fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}
fn valid_expected(e: &Expected, id: &str, file: &str) -> bool {
    !e.manager_id.is_empty() && decimal(&e.revision) && e.session_id == id && e.session_file == file
}
fn validate_endpoint(e: &Endpoint) -> Result<(), Error> {
    if e.protocol_version != 2
        || e.pid == 0
        || !decimal(&e.process_starttime)
        || e.instance_nonce.0.len() != 64
        || !e
            .instance_nonce
            .0
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || e.expected
            .as_ref()
            .is_some_and(|x| !valid_expected(x, &e.session_id, &e.session_file))
    {
        return Err(invalid());
    }
    Ok(())
}
unsafe extern "C" {
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn connect(fd: i32, addr: *const std::ffi::c_void, len: u32) -> i32;
    pub safe fn getuid() -> u32;
    fn getsockopt(
        fd: i32,
        level: i32,
        name: i32,
        value: *mut std::ffi::c_void,
        len: *mut u32,
    ) -> i32;
}
fn uid() -> u32 {
    getuid()
}
// Open every component relative to an already pinned directory, without symlinks.
fn directory(path: &Path) -> Result<File, Error> {
    if !path.is_absolute() {
        return Err(Error::UnsafePath);
    }
    let mut file = File::open("/")?;
    for part in path.components().skip(1) {
        let std::path::Component::Normal(name) = part else {
            return Err(Error::UnsafePath);
        };
        file = OpenOptions::new()
            .read(true)
            .custom_flags(NOFOLLOW | DIRECTORY)
            .open(PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd())).join(name))?;
        let m = file.metadata()?;
        if (m.uid() != uid() && m.uid() != 0) || m.mode() & 0o022 != 0 {
            return Err(Error::UnsafePath);
        }
    }
    let m = file.metadata()?;
    if m.uid() != uid() || m.mode() & 0o777 != 0o700 {
        return Err(Error::UnsafePath);
    }
    Ok(file)
}
fn bounded(file: File) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    file.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > LIMIT {
        return Err(invalid());
    }
    Ok(bytes)
}
/// Bounded private manifest discovery. Invalid candidates are ignored, never downgraded.
pub fn discover(runtime: &Path) -> Result<Vec<Endpoint>, Error> {
    let root = directory(runtime)?;
    let pinned = PathBuf::from(format!("/proc/self/fd/{}", root.as_raw_fd()));
    let mut result = Vec::new();
    for entry in fs::read_dir(&pinned)?.take(128) {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with("ac-") {
            continue;
        }
        let candidate = || -> Result<Endpoint, Error> {
            // Validate via the original absolute path as well as pinned file handles.
            let dir = directory(&runtime.join(entry.file_name()))?;
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(NOFOLLOW)
                .open(format!("/proc/self/fd/{}/manifest.json", dir.as_raw_fd()))?;
            let m = file.metadata()?;
            if !m.is_file() || m.uid() != uid() || m.mode() & 0o777 != 0o600 {
                return Err(Error::UnsafePath);
            }
            let e: Endpoint = serde_json::from_slice(&bounded(file)?).map_err(|_| invalid())?;
            validate_endpoint(&e)?;
            if e.socket != runtime.join(entry.file_name()).join("s") {
                return Err(Error::UnsafePath);
            }
            process(&e)?;
            Ok(e)
        };
        if let Ok(e) = candidate() {
            result.push(e);
        }
    }
    Ok(result)
}
fn process(e: &Endpoint) -> Result<(), Error> {
    let p = PathBuf::from(format!("/proc/{}", e.pid));
    if fs::metadata(&p)?.uid() != uid() {
        return Err(Error::Identity);
    }
    let bytes = bounded(File::open(p.join("stat"))?)?;
    let stat =
        crate::procfs::stat::parse_stat(std::str::from_utf8(&bytes).map_err(|_| Error::Identity)?)
            .map_err(|_| Error::Identity)?;
    if stat.pid != e.pid || stat.starttime.to_string() != e.process_starttime {
        return Err(Error::Identity);
    }
    Ok(())
}
/// One connection, one JSONL request. Any failure after write begins is Unknown:
/// no retries; reconcile with request.status() using the same operation ID.
pub fn exchange(endpoint: &Endpoint, request: &Request, timeout: Duration) -> Result<Reply, Error> {
    let started = Instant::now();
    validate_endpoint(endpoint)?;
    if timeout.is_zero()
        || timeout > Duration::from_secs(5)
        || request.pid != endpoint.pid
        || request.process_starttime != endpoint.process_starttime
        || request.instance_nonce.0 != endpoint.instance_nonce.0
        || request.session_id != endpoint.session_id
        || request.session_file != endpoint.session_file
        || !match request.protocol_version {
            2 => request.action != Action::HandoffPickup,
            3 => matches!(
                request.action,
                Action::HandoffPickup | Action::Status | Action::Cancel
            ),
            _ => false,
        }
        || !uuid(&request.operation_id)
        || !valid_expected(
            &request.expected,
            &request.session_id,
            &request.session_file,
        )
    {
        return Err(invalid());
    }
    let deadline = started + timeout;
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(Error::Io(io::ErrorKind::TimedOut))
    };
    remaining()?;
    let dir = directory(endpoint.socket.parent().ok_or(Error::UnsafePath)?)?;
    let socket = PathBuf::from(format!("/proc/self/fd/{}/s", dir.as_raw_fd()));
    let m = fs::symlink_metadata(&socket)?;
    if !m.file_type().is_socket()
        || m.uid() != uid()
        || m.mode() & 0o777 != 0o600
        || endpoint.socket.file_name() != Some(std::ffi::OsStr::new("s"))
    {
        return Err(Error::UnsafePath);
    }
    process(endpoint)?;
    remaining()?;
    let path = socket.as_os_str().as_bytes();
    if path.len() >= 108 || path.contains(&0) {
        return Err(Error::UnsafePath);
    }
    #[repr(C)]
    struct Addr {
        family: u16,
        path: [u8; 108],
    }
    let mut addr = Addr {
        family: 1,
        path: [0; 108],
    };
    addr.path[..path.len()].copy_from_slice(path);
    // Linux NONBLOCK | CLOEXEC: a saturated accept queue must never block.
    let fd = unsafe { self::socket(1, 1 | 0o4000 | 0o2000000, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut stream = unsafe { UnixStream::from_raw_fd(fd) };
    if unsafe {
        connect(
            fd,
            (&addr as *const Addr).cast(),
            (2 + path.len() + 1) as u32,
        )
    } != 0
    {
        return Err(io::Error::last_os_error().into());
    }
    #[repr(C)]
    struct Cred {
        pid: i32,
        uid: u32,
        gid: u32,
    }
    let mut cred = Cred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<Cred>() as u32;
    let rc = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            1,
            17,
            (&mut cred as *mut Cred).cast(),
            &mut len,
        )
    };
    if rc != 0
        || len as usize != std::mem::size_of::<Cred>()
        || cred.pid as u32 != endpoint.pid
        || cred.uid != uid()
    {
        return Err(Error::Identity);
    }
    process(endpoint)?;
    let mut bytes = serde_json::to_vec(request).map_err(|_| invalid())?;
    bytes.push(b'\n');
    if bytes.len() > LIMIT {
        return Err(invalid());
    }
    remaining()?;
    // Once the first send is attempted, every failure is ambiguous. Only resume
    // partial nonblocking I/O on this connection; never reconnect or resend.
    let pause = || -> Result<(), Error> {
        std::thread::sleep(remaining()?.min(Duration::from_millis(1)));
        Ok(())
    };
    let mut offset = 0;
    while offset < bytes.len() {
        match stream.write(&bytes[offset..]) {
            Ok(0) => return Err(Error::Unknown),
            Ok(n) => offset += n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                pause().map_err(|_| Error::Unknown)?;
            }
            Err(_) => return Err(Error::Unknown),
        }
        remaining().map_err(|_| Error::Unknown)?;
    }
    let mut bytes = Vec::new();
    loop {
        remaining().map_err(|_| Error::Unknown)?;
        let mut chunk = [0; 1024];
        let n = match stream.read(&mut chunk) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                pause().map_err(|_| Error::Unknown)?;
                continue;
            }
            Err(_) => return Err(Error::Unknown),
        };
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() > LIMIT {
            return Err(Error::Unknown);
        }
    }
    if bytes.len() > LIMIT
        || bytes.last() != Some(&b'\n')
        || bytes[..bytes.len() - 1].contains(&b'\n')
    {
        return Err(Error::Unknown);
    }
    let reply: Reply = serde_json::from_slice(&bytes).map_err(|_| Error::Unknown)?;
    if unsupported_protocol(&reply, request) {
        return Err(Error::UnsupportedProtocol);
    }
    validate_reply(&reply, request).map_err(|_| Error::Unknown)?;
    Ok(reply)
}
const CODES: &[&str] = &[
    "accepted",
    "compacting",
    "generating",
    "persisted",
    "compacted",
    "invalid_request",
    "incompatible",
    "unauthorized",
    "stale_instance",
    "stale_revision",
    "unsupported_guarded_compaction",
    "busy",
    "disabled",
    "capacity",
    "duplicate_mismatch",
    "expired",
    "unknown_operation",
    "invalid",
    "stale",
    "pending",
    "non_persistent",
    "cancelled",
    "failed",
    "timeout",
    "io_error",
    "model_error",
];
/// The exact refusal a CC1 listener sends to a protocol 3 request: version 2,
/// `invalid_request` (unknown action) or `incompatible` (unknown version), with
/// no metadata or result. A current listener echoes version 3 instead.
fn unsupported_protocol(r: &Reply, q: &Request) -> bool {
    q.protocol_version == 3
        && r.protocol_version == 2
        && r.operation_id == q.operation_id
        && r.state == State::Rejected
        && ["invalid_request", "incompatible"].contains(&r.code.as_str())
        && r.metadata.is_none()
        && r.pickup.is_none()
        && r.artifact_path.is_none()
        && r.expected.is_none()
        && r.compaction.is_none()
        && r.completed_at.is_none()
        && r.cost_usd.is_none()
}
fn validate_reply(r: &Reply, q: &Request) -> Result<(), Error> {
    if r.protocol_version != q.protocol_version
        || r.operation_id != q.operation_id
        || !(CODES.contains(&r.code.as_str())
            || (q.protocol_version == 3 && PICKUP_CODES.contains(&r.code.as_str())))
    {
        return Err(invalid());
    }
    if let Some(p) = &r.pickup {
        return validate_pickup(r, p, q);
    }
    // An arm reply without the record is a refusal before any effect: the
    // listener's (no metadata) or dispatch's (with metadata). Accepted or
    // running arms always carry the record.
    if q.action == Action::HandoffPickup
        && !((r.state == State::Rejected && ARM_REFUSALS.contains(&r.code.as_str()))
            || (r.state == State::Unknown && r.code == "expired"))
    {
        return Err(invalid());
    }
    if let Some(m) = &r.metadata
        && let Some(e) = &m.expected
        && (!valid_expected(e, &q.session_id, &q.session_file)
            || e.manager_id != q.expected.manager_id)
    {
        return Err(invalid());
    }
    if (r.state == State::Accepted && r.code != "accepted")
        || (r.state == State::Running && !["compacting", "generating"].contains(&r.code.as_str()))
    {
        return Err(invalid());
    }
    if r.cost_usd.is_some_and(|v| !v.is_finite() || v < 0.0) {
        return Err(invalid());
    }
    if r.state == State::Completed {
        if r.completed_at
            .as_ref()
            .is_none_or(|v| v.len() != 24 || !v.ends_with('Z'))
        {
            return Err(invalid());
        }
        if r.metadata.is_none() {
            return Err(invalid());
        }
        match r.code.as_str() {
            "persisted"
                if q.action != Action::Compact
                    && r.expected.as_ref() == Some(&q.expected)
                    && r.artifact_path.as_ref().is_some_and(|p| {
                        p.is_absolute()
                            && p.file_name().is_some_and(|n| {
                                n.to_string_lossy()
                                    .ends_with(&format!("-{}.md", q.operation_id))
                            })
                    })
                    && r.compaction.is_none() => {}
            "compacted" if q.action != Action::Handoff => {
                let c = r.compaction.as_ref().ok_or_else(invalid)?;
                if c.capability != "agentcenter.compactGuarded.v1"
                    || c.session_id != q.session_id
                    || c.session_file != q.session_file
                    || c.previous_leaf_id != q.expected.leaf_id
                    || c.entry_id.is_empty()
                    || r.artifact_path.is_some()
                {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
    } else if r.artifact_path.is_some() || r.compaction.is_some() || r.expected.is_some() {
        return Err(invalid());
    }
    Ok(())
}

// ---- Protocol 3: handoff-pickup (CC5) ----
//
// The socket only arms a request; Pi exposes newSession to command handlers
// only, so the command is typed through validated Herdr (`agent.prompt`,
// protocol 22). A Pi outside Herdr has no pickup capability. Herdr reports a
// Pi pane's session as its file (`agent_session.kind == "path"`), never the
// session ID, so the pane must report exactly the endpoint's session file.

/// Codes protocol 3 adds to [`CODES`] for pickup records.
const PICKUP_CODES: &[&str] = &[
    "armed",
    "arm_expired",
    "handoff_persisted",
    "new_session_requested",
    "new_session_confirmed",
    "new_session_cancelled",
    "new_session_failed",
    "new_session_unconfirmed",
    "parent_mismatch",
    "pickup_sent",
    "pickup_loaded",
    "pickup_failed",
    "pickup_unconfirmed",
];
/// Codes an arm may be refused with, before any effect and without a record.
const ARM_REFUSALS: &[&str] = &[
    "invalid_request",
    "incompatible",
    "unauthorized",
    "stale_instance",
    "stale_revision",
    "unsupported_guarded_compaction",
    "busy",
    "disabled",
    "capacity",
    "duplicate_mismatch",
];
/// Registered by the extension; the argument is the armed operation UUID.
pub const PICKUP_COMMAND: &str = "/agentcenter-pickup";
/// Disabled reason when the Pi session has no validated Herdr pane.
pub const NO_HERDR_PANE: &str = "no_herdr_pane";
/// The Pi runs in a Herdr pane that reports no agent session (Herdr's Pi
/// integration is not loaded): the pane cannot be tied to this session.
pub const NO_HERDR_SESSION: &str = "no_herdr_session";
/// The Pi extension speaks only protocol 2 (CC1): no pickup there.
pub const UNSUPPORTED_PROTOCOL: &str = "unsupported_protocol";
/// Handoff disabled reasons the extension publishes; any other is "disabled".
const HANDOFF_REASONS: &[&str] = &[
    "busy",
    "closed",
    "no_model",
    "model_auth_required",
    "unsupported_guarded_compaction",
];

fn stage(s: &str) -> Option<pickup::Stage> {
    Some(match s {
        "handoff_running" => pickup::Stage::HandoffRunning,
        "handoff_persisted" => pickup::Stage::HandoffPersisted,
        "clearing" => pickup::Stage::Clearing,
        "new_session_confirmed" => pickup::Stage::NewSessionConfirmed,
        "pickup_sent" => pickup::Stage::PickupSent,
        "pickup_loaded" => pickup::Stage::PickupLoaded,
        _ => return None,
    })
}
fn sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}
fn validate_pickup(r: &Reply, p: &PickupReply, q: &Request) -> Result<(), Error> {
    use pickup::Stage::*;
    let stage = stage(&p.stage).ok_or_else(invalid)?;
    if q.protocol_version != 3
        || r.metadata.is_none()
        || r.artifact_path.is_some()
        || r.compaction.is_some()
        || r.expected.is_some()
        || p.old_session.session_id.is_empty()
        || !Path::new(&p.old_session.session_file).is_absolute()
        // The arming endpoint is the old session; later queries come from any endpoint.
        || (q.action == Action::HandoffPickup
            && (p.old_session.session_id != q.session_id
                || p.old_session.session_file != q.session_file))
        || p.handoff.is_some() != (stage >= HandoffPersisted)
        || p.new_session.is_some() != (stage >= NewSessionConfirmed)
        || p.loaded_entry_id.is_some() != (stage == PickupLoaded)
        || p.loaded_entry_id.as_ref().is_some_and(String::is_empty)
    {
        return Err(invalid());
    }
    if let Some(h) = &p.handoff
        && (!h.path.is_absolute()
            || !sha256_hex(&h.sha256)
            || !h.path.file_name().is_some_and(|n| {
                n.to_string_lossy()
                    .ends_with(&format!("-{}.md", q.operation_id))
            }))
    {
        return Err(invalid());
    }
    if let Some(n) = &p.new_session
        && (n.session_id.is_empty()
            || n.session_id == p.old_session.session_id
            // A mismatch is reported with whatever the new session showed.
            || (r.code != "parent_mismatch"
                && (n.parent_session.as_deref() != Some(&p.old_session.session_file)
                    || !n
                        .session_file
                        .as_deref()
                        .is_some_and(|f| Path::new(f).is_absolute()))))
    {
        return Err(invalid());
    }
    let code = r.code.as_str();
    let coherent = match r.state {
        State::Accepted => code == "armed" && stage == HandoffRunning,
        State::Running => [
            "generating",
            "handoff_persisted",
            "new_session_requested",
            "new_session_confirmed",
            "pickup_sent",
        ]
        .contains(&code),
        State::Completed => code == "pickup_loaded" && stage == PickupLoaded,
        _ => code != "pickup_loaded" && code != "armed",
    };
    if !coherent {
        return Err(invalid());
    }
    Ok(())
}

/// Evidence a [`pickup::Progress`] has no field for: where the new session
/// lives and which entry loaded the handoff. Paths and IDs only.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PickupEvidence {
    pub new_session_file: Option<PathBuf>,
    pub parent_session: Option<PathBuf>,
    pub loaded_entry_id: Option<String>,
}

/// Converts a validated protocol 3 reply that carries a pickup record. The
/// handoff file is only stat'ed (size, mtime), never opened.
pub fn progress(r: &Reply) -> Result<(pickup::Progress, PickupEvidence), Error> {
    let p = r.pickup.as_ref().ok_or(Error::Unknown)?;
    let code = CODES
        .iter()
        .chain(PICKUP_CODES)
        .find(|c| **c == r.code)
        .copied()
        .ok_or(Error::Unknown)?;
    let handoff = match &p.handoff {
        None => None,
        Some(h) => {
            let m = fs::symlink_metadata(&h.path)?;
            if !m.is_file() || m.len() != h.size {
                return Err(Error::Unknown);
            }
            let modified_ms = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64);
            Some(super::Artifact {
                path: h.path.clone(),
                size: h.size,
                sha256: h.sha256.clone(),
                modified_ms,
            })
        }
    };
    let progress = pickup::Progress {
        operation_id: r.operation_id.clone(),
        state: r.state,
        stage: stage(&p.stage).ok_or(Error::Unknown)?,
        code,
        old_session: pickup::OldSession {
            session_id: p.old_session.session_id.clone(),
            session_file: PathBuf::from(&p.old_session.session_file),
        },
        handoff,
        new_session_id: p.new_session.as_ref().map(|n| n.session_id.clone()),
    };
    let evidence = PickupEvidence {
        new_session_file: p
            .new_session
            .as_ref()
            .and_then(|n| n.session_file.as_ref().map(PathBuf::from)),
        parent_session: p
            .new_session
            .as_ref()
            .and_then(|n| n.parent_session.as_ref().map(PathBuf::from)),
        loaded_entry_id: p.loaded_entry_id.clone(),
    };
    Ok((progress, evidence))
}

/// Why pickup is unavailable for this endpoint, or `None` when it can be
/// offered: it needs a Herdr pane whose process is this exact Pi instance, and
/// the handoff it starts with must be enabled. The v2 manifest never carries
/// `handoff_pickup` (only protocol 3 replies do), so the manifest's `handoff`
/// capability is the discovery-time signal.
pub fn pickup_unavailable(
    endpoint: &Endpoint,
    target: Option<&herdr::Target>,
) -> Option<&'static str> {
    let Some(t) = target else {
        return Some(NO_HERDR_PANE);
    };
    if t.pid != endpoint.pid || t.starttime.to_string() != endpoint.process_starttime {
        return Some("identity");
    }
    let handoff = &endpoint.capabilities.handoff;
    if handoff.enabled {
        return None;
    }
    let reason = handoff.disabled_reason.as_deref();
    Some(
        HANDOFF_REASONS
            .iter()
            .find(|r| Some(**r) == reason)
            .copied()
            .unwrap_or("disabled"),
    )
}

#[derive(Debug)]
pub enum PickupError {
    /// Not offered (no Herdr pane, other process, handoff disabled, no Herdr
    /// agent session, CC1-only extension): nothing ran.
    Unavailable(&'static str),
    /// Pi transport/validation failed while arming; an arm may exist and expires alone.
    Pi(Error),
    /// The arm was refused (busy, stale, disabled...): no effect.
    Refused(Box<Reply>),
    /// Herdr refused before typing. Before the arm (`cancelled: None`, nothing
    /// armed) or after it, when the arm was cancelled (its reply, if any; a
    /// failed cancel leaves the arm to expire alone). A failure once the prompt
    /// was written is [`PickupError::Unknown`] and is never cancelled.
    Herdr {
        error: herdr::Error,
        cancelled: Option<Box<Reply>>,
    },
    /// Herdr may have typed the command: query the same operation, never retry.
    Unknown(Box<Request>),
}

/// One click: read the pane once (it must report an agent session at all),
/// arm over the socket, then type `/agentcenter-pickup <op>` into the pane
/// through Herdr, which revalidates pane, process starttime, adapter, `idle`
/// and the session file immediately before the prompt. Returns the retained
/// request for [`pickup_status`]; completion only ever comes from status
/// evidence.
pub fn handoff_pickup(
    endpoint: &Endpoint,
    herdr: &herdr::Endpoint,
    target: Option<&herdr::Target>,
    operation_id: String,
    timeout: Duration,
) -> Result<(Request, Reply), PickupError> {
    if let Some(reason) = pickup_unavailable(endpoint, target) {
        return Err(PickupError::Unavailable(reason));
    }
    let target = target.ok_or(PickupError::Unavailable(NO_HERDR_PANE))?;
    let session = herdr::AgentSession::Path(Path::new(&endpoint.session_file));
    match herdr.pane_reported(target, timeout) {
        Ok((None, _)) => return Err(PickupError::Unavailable(NO_HERDR_SESSION)),
        Ok(_) => {}
        Err(error) => {
            return Err(PickupError::Herdr {
                error,
                cancelled: None,
            });
        }
    }
    let mut request = endpoint
        .request(operation_id, Action::HandoffPickup)
        .map_err(PickupError::Pi)?;
    request.protocol_version = 3;
    let armed = exchange(endpoint, &request, timeout).map_err(|e| match e {
        Error::UnsupportedProtocol => PickupError::Unavailable(UNSUPPORTED_PROTOCOL),
        e => PickupError::Pi(e),
    })?;
    if armed.state != State::Accepted || armed.code != "armed" {
        return Err(PickupError::Refused(Box::new(armed)));
    }
    let text = format!("{PICKUP_COMMAND} {}", request.operation_id);
    match herdr.prompt(target, session, &text, timeout) {
        Ok(()) => Ok((request, armed)),
        Err(herdr::Error::Unknown) => Err(PickupError::Unknown(Box::new(request))),
        Err(error) => Err(PickupError::Herdr {
            error,
            cancelled: exchange(endpoint, &request.cancel(), timeout)
                .ok()
                .map(Box::new),
        }),
    }
}

/// Status of a pickup by operation ID on whichever endpoint the same process
/// (pid + starttime) publishes now: the session and nonce rotate on success.
pub fn pickup_status(
    runtime: &Path,
    pid: u32,
    process_starttime: &str,
    operation_id: &str,
    timeout: Duration,
) -> Result<(pickup::Progress, PickupEvidence), Error> {
    let found: Vec<Endpoint> = discover(runtime)?
        .into_iter()
        .filter(|e| e.pid == pid && e.process_starttime == process_starttime)
        .collect();
    let [endpoint] = found.as_slice() else {
        return Err(Error::Identity);
    };
    let mut request = endpoint.request(operation_id.to_owned(), Action::Status)?;
    request.protocol_version = 3;
    progress(&exchange(endpoint, &request, timeout)?)
}

#[cfg(test)]
#[path = "pi_tests.rs"]
mod tests;

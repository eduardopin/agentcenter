//! Protocol 2 client. Node authenticates clients with kernel DAC (0700 directory,
//! 0600 socket) and nonce, not a nonexistent Node peercred getter. This client
//! additionally verifies the server's Linux SO_PEERCRED and /proc starttime.
//! Same-UID processes and root are not isolated by this protocol.
pub use super::{Capability, State};
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
}
#[derive(Debug)]
pub enum Error {
    Io(io::ErrorKind),
    Invalid,
    UnsafePath,
    Identity,
    Unknown,
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
        || request.protocol_version != 2
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
    validate_reply(&reply, request).map_err(|_| Error::Unknown)?;
    Ok(reply)
}
fn validate_reply(r: &Reply, q: &Request) -> Result<(), Error> {
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
    if r.protocol_version != 2
        || r.operation_id != q.operation_id
        || !CODES.contains(&r.code.as_str())
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

#[cfg(test)]
#[path = "pi_tests.rs"]
mod tests;

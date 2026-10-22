//! Explicit pane focus and agent prompt, protocol 22. No current-pane or default
//! target. Client revalidation is not a server-side CAS: Herdr has no process or
//! session precondition on pane.focus or agent.prompt, and it answers one request
//! per connection, so revalidation and effect are separate connections to the
//! same pinned server (peer pid and starttime rechecked on each). Completion of
//! focus means pane focus, not desktop-window activation; a prompt reply only
//! means Herdr typed it, never that the agent acted on it.
use crate::overlay::herdr::{adapter_id, client::MAX_LINE, parse_agent_list, parse_process_info};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, MetadataExt},
            net::UnixStream,
        },
    },
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub socket: PathBuf,
    pub server_pid: u32,
    pub server_starttime: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub pid: u32,
    pub starttime: u64,
    pub adapter: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
}
impl Target {
    pub fn from_session(s: &crate::session::Session) -> Option<Self> {
        let h = s.overlay.as_ref()?.herdr.as_ref()?;
        Some(Self {
            pid: s.pid,
            starttime: s.starttime?,
            adapter: s.agent.clone(),
            workspace_id: h.workspace_id.clone()?,
            tab_id: h.tab_id.clone()?,
            pane_id: h.pane_id.clone()?,
        })
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Identity,
    Stale,
    Ambiguous,
    /// Agent status is not `idle` (working, blocked in a dialog, unknown).
    Busy,
    /// The pane reports no agent session or a different one.
    Session,
    UnsafeSocket,
    Transport,
    Unknown,
}
unsafe extern "C" {
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn connect(fd: i32, addr: *const std::ffi::c_void, len: u32) -> i32;
    fn getsockopt(
        fd: i32,
        level: i32,
        name: i32,
        value: *mut std::ffi::c_void,
        len: *mut u32,
    ) -> i32;
}
fn process(pid: u32, start: u64) -> Result<(), Error> {
    let p = PathBuf::from(format!("/proc/{pid}"));
    if pid == 0 || fs::metadata(&p).map_err(|_| Error::Identity)?.uid() != super::pi::getuid() {
        return Err(Error::Identity);
    }
    let text = fs::read_to_string(p.join("stat")).map_err(|_| Error::Identity)?;
    let s = crate::procfs::stat::parse_stat(&text).map_err(|_| Error::Identity)?;
    if s.pid != pid || s.starttime != start {
        return Err(Error::Identity);
    }
    Ok(())
}
fn remaining(end: Instant) -> Result<(), Error> {
    if Instant::now() >= end {
        Err(Error::Transport)
    } else {
        Ok(())
    }
}
impl Endpoint {
    fn open(&self, end: Instant) -> Result<UnixStream, Error> {
        remaining(end)?;
        // Reject symlinks and writable ancestors. /tmp is deliberately not an
        // acceptable production endpoint; isolated tests use a private HOME dir.
        if !self.socket.is_absolute() {
            return Err(Error::UnsafeSocket);
        }
        for p in self.socket.ancestors() {
            let m = fs::symlink_metadata(p).map_err(|_| Error::UnsafeSocket)?;
            if m.file_type().is_symlink()
                || (m.uid() != super::pi::getuid() && m.uid() != 0)
                || m.mode() & 0o022 != 0
            {
                return Err(Error::UnsafeSocket);
            }
        }
        let m = fs::symlink_metadata(&self.socket).map_err(|_| Error::UnsafeSocket)?;
        if !m.file_type().is_socket() || m.uid() != super::pi::getuid() || m.mode() & 0o077 != 0 {
            return Err(Error::UnsafeSocket);
        }
        process(self.server_pid, self.server_starttime)?;
        let path = self.socket.as_os_str().as_bytes();
        if path.len() >= 108 || path.contains(&0) {
            return Err(Error::UnsafeSocket);
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
        // Linux nonblocking connect: a full accept queue refuses immediately.
        // Never use the overlay's blocking connect for a control operation.
        let fd = unsafe { socket(1, 1 | 0o4000 | 0o2000000, 0) };
        if fd < 0 {
            return Err(Error::Transport);
        }
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        if unsafe {
            connect(
                fd,
                (&addr as *const Addr).cast(),
                (2 + path.len() + 1) as u32,
            )
        } != 0
        {
            return Err(Error::Transport);
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
        if unsafe {
            getsockopt(
                stream.as_raw_fd(),
                1,
                17,
                (&mut cred as *mut Cred).cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of::<Cred>()
            || cred.uid != super::pi::getuid()
            || cred.pid as u32 != self.server_pid
        {
            return Err(Error::Identity);
        }
        process(self.server_pid, self.server_starttime)?;
        Ok(stream)
    }
    fn request(
        &self,
        method: &str,
        params: Value,
        end: Instant,
        effect: bool,
    ) -> Result<Vec<u8>, Error> {
        let mut s = self.open(end)?;
        let mut bytes = serde_json::to_vec(
            &json!({"id":"agentcenter-control","method":method,"params":params}),
        )
        .map_err(|_| Error::Invalid)?;
        bytes.push(b'\n');
        let exchange = || -> Result<Vec<u8>, Error> {
            let mut offset = 0;
            while offset < bytes.len() {
                remaining(end)?;
                match s.write(&bytes[offset..]) {
                    Ok(0) => return Err(Error::Transport),
                    Ok(n) => offset += n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(_) => return Err(Error::Transport),
                }
            }
            let mut out = Vec::new();
            loop {
                remaining(end)?;
                let mut chunk = [0; 4096];
                match s.read(&mut chunk) {
                    Ok(0) => return Err(Error::Transport),
                    Ok(n) => out.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(_) => return Err(Error::Transport),
                }
                if out.len() > MAX_LINE {
                    return Err(Error::Invalid);
                }
                if out.contains(&b'\n') {
                    let v: Value = serde_json::from_slice(&out).map_err(|_| Error::Invalid)?;
                    if v["id"] != "agentcenter-control"
                        || v.get("error").is_some()
                        || v.get("result").is_none()
                    {
                        return Err(Error::Invalid);
                    }
                    return Ok(out);
                }
            }
        };
        let mut exchange = exchange;
        exchange().map_err(|e| if effect { Error::Unknown } else { e })
    }
    fn validate(&self, t: &Target, end: Instant) -> Result<(), Error> {
        process(t.pid, t.starttime)?;
        let list = self.request("agent.list", json!({}), end, false)?;
        let agents = parse_agent_list(&list).ok_or(Error::Invalid)?;
        if agents.len() > 128 {
            return Err(Error::Invalid);
        }
        let mut matches = 0;
        for a in agents {
            if a.agent.as_deref().map(adapter_id) != Some(t.adapter.as_str()) {
                continue;
            }
            let line = self.request(
                "pane.process_info",
                json!({"pane_id":a.pane_id}),
                end,
                false,
            )?;
            let p = parse_process_info(&line).ok_or(Error::Invalid)?;
            if p.pane_id != a.pane_id {
                return Err(Error::Stale);
            }
            if p.pids.contains(&t.pid) || p.shell_pid == Some(t.pid) {
                matches += 1;
                if a.pane_id != t.pane_id
                    || a.workspace_id != t.workspace_id
                    || a.tab_id != t.tab_id
                {
                    return Err(Error::Stale);
                }
            }
        }
        if matches > 1 {
            return Err(Error::Ambiguous);
        }
        if matches != 1 {
            return Err(Error::Stale);
        }
        process(t.pid, t.starttime)
    }
    /// Global deadline includes all reads and the effect. No automatic retry.
    pub fn focus(&self, t: &Target, timeout: Duration) -> Result<(), Error> {
        let end = inputs(t, timeout)?;
        self.validate(t, end)?;
        // A fresh snapshot, not cached sampler metadata, immediately before focus.
        self.validate(t, end)?;
        let line = self.request("pane.focus", json!({"pane_id": t.pane_id}), end, true)?;
        let v: Value = serde_json::from_slice(&line).map_err(|_| Error::Unknown)?;
        if v["result"]["type"] != "pane_focused"
            || v["result"]["pane_id"] != t.pane_id
            || v["result"]["workspace_id"] != t.workspace_id
        {
            return Err(Error::Unknown);
        }
        Ok(())
    }
    /// `agent.get` of one pane: only identity, status and session ID are read.
    fn agent_get(&self, pane_id: &str, end: Instant) -> Result<AgentInfo, Error> {
        let line = self.request("agent.get", json!({"target": pane_id}), end, false)?;
        let v: Value = serde_json::from_slice(&line).map_err(|_| Error::Invalid)?;
        if v["result"]["type"] != "agent_info" {
            return Err(Error::Invalid);
        }
        let a = &v["result"]["agent"];
        let text = |k: &str| a[k].as_str().map(str::to_string);
        let session = &a["agent_session"];
        Ok(AgentInfo {
            agent: text("agent"),
            status: text("agent_status"),
            session_id: (session["kind"] == "id")
                .then(|| session["value"].as_str().map(str::to_string))
                .flatten(),
            pane_id: text("pane_id"),
            workspace_id: text("workspace_id"),
            tab_id: text("tab_id"),
        })
    }
    /// Fresh reads only: /proc identity, a single pane holding the pid
    /// (`agent.list` + `pane.process_info`), then `agent.get` of that pane for
    /// agent type, explicit location, session ID and `idle`.
    fn agent_checks(&self, t: &Target, session_id: &str, end: Instant) -> Result<(), Error> {
        self.validate(t, end)?;
        let a = self.agent_get(&t.pane_id, end)?;
        if a.pane_id.as_deref() != Some(t.pane_id.as_str())
            || a.workspace_id.as_deref() != Some(t.workspace_id.as_str())
            || a.tab_id.as_deref() != Some(t.tab_id.as_str())
            || a.agent.as_deref().map(adapter_id) != Some(t.adapter.as_str())
        {
            return Err(Error::Stale);
        }
        if a.session_id.as_deref() != Some(session_id) {
            return Err(Error::Session);
        }
        if a.status.as_deref() != Some("idle") {
            return Err(Error::Busy);
        }
        process(t.pid, t.starttime)
    }
    /// Read-only capability probe for an agent pane bound to `session_id`.
    pub fn check_agent(
        &self,
        t: &Target,
        session_id: &str,
        timeout: Duration,
    ) -> Result<(), Error> {
        let end = inputs(t, timeout)?;
        if session_id.is_empty() {
            return Err(Error::Invalid);
        }
        self.agent_checks(t, session_id, end)
    }
    /// Revalidates everything immediately before one `agent.prompt`; any
    /// divergence returns before the effect. Crate-private: the text is closed
    /// by the adapter, never taken from a client. Failure after the write
    /// begins is [`Error::Unknown`]; there is no retry.
    pub(crate) fn prompt(
        &self,
        t: &Target,
        session_id: &str,
        text: &str,
        timeout: Duration,
    ) -> Result<(), Error> {
        let end = inputs(t, timeout)?;
        if session_id.is_empty() || text.is_empty() || text.chars().any(char::is_control) {
            return Err(Error::Invalid);
        }
        self.agent_checks(t, session_id, end)?;
        let line = self.request(
            "agent.prompt",
            json!({"target": t.pane_id, "text": text}),
            end,
            true,
        )?;
        let v: Value = serde_json::from_slice(&line).map_err(|_| Error::Unknown)?;
        if v["result"]["type"] != "agent_prompted" || v["result"]["agent"]["pane_id"] != t.pane_id {
            return Err(Error::Unknown);
        }
        Ok(())
    }
}
struct AgentInfo {
    agent: Option<String>,
    status: Option<String>,
    session_id: Option<String>,
    pane_id: Option<String>,
    workspace_id: Option<String>,
    tab_id: Option<String>,
}
fn inputs(t: &Target, timeout: Duration) -> Result<Instant, Error> {
    if timeout.is_zero()
        || timeout > Duration::from_secs(5)
        || [&t.adapter, &t.workspace_id, &t.tab_id, &t.pane_id]
            .iter()
            .any(|s| s.is_empty() || matches!(s.as_str(), "current" | "default"))
    {
        return Err(Error::Invalid);
    }
    Ok(Instant::now() + timeout)
}

#[cfg(test)]
#[path = "herdr_tests.rs"]
mod tests;

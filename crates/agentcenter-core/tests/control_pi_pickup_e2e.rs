//! E2E against a REAL private Pi in a REAL, isolated Herdr: the CC5
//! handoff-pickup through the Rust client (`pi::handoff_pickup` +
//! `pi::pickup_status`) to `pickup_loaded`, and the same click with a draft or
//! the `/` autocomplete open in the Pi editor. All run on the same pane, one
//! after the other (`--test-threads=1`); each discovers the Pi endpoint and
//! reads the pane from Herdr as they are now (a pickup rotates the session).
//! `#[ignore]`: they only run under `scripts/e2e-pi-herdr.sh`, which builds the
//! isolated HOME, runtime directory, Herdr server, API stub and agent pane and
//! exports the `AC_E2E_*` variables read here. Without them it fails, so a
//! stray `--ignored` run can never pass with nothing tested.
use agentcenter_core::control::{
    State, herdr,
    pi::{self, PickupError},
    pickup::{Progress, Stage},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{ffi::OsStrExt, net::UnixStream},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const OP: &str = "e2e0c5aa-0000-4000-8000-00000000c5c5";
const DRAFT_OP: &str = "e2e0c5bb-0000-4000-8000-00000000c5c5";
const SLASH_OP: &str = "e2e0c5cc-0000-4000-8000-00000000c5c5";
const CANCEL_OP: &str = "e2e0c5dd-0000-4000-8000-00000000c5c5";
/// The stub flags any request whose messages contain this text.
const DRAFT: &str = "e2e-draft-not-a-command";
const TIMEOUT: Duration = Duration::from_secs(5);

fn var(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("{k} unset: run scripts/e2e-pi-herdr.sh"))
}
fn starttime(pid: u32) -> u64 {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest = &s[s.rfind(')').unwrap() + 2..];
    rest.split(' ').nth(19).unwrap().parse().unwrap()
}
/// One raw request to the isolated Herdr, reply line as JSON (reads and the
/// test's own keystrokes; the client never goes through here).
fn raw(method: &str, params: Value) -> Value {
    let mut s = UnixStream::connect(var("AC_E2E_SOCKET")).unwrap();
    let mut q = serde_json::to_vec(&json!({"id":"e2e","method":method,"params":params})).unwrap();
    q.push(b'\n');
    s.write_all(&q).unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
fn screen() -> String {
    let r = raw(
        "pane.read",
        json!({"pane_id": var("AC_E2E_PANE"), "source": "visible"}),
    );
    r["result"]["read"]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}
/// The stub's request log: one JSON object per model request.
fn stub_log() -> Vec<Value> {
    fs::read_to_string(var("AC_E2E_STUB_LOG"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

struct Arrangement {
    runtime: PathBuf,
    endpoint: pi::Endpoint,
    server: herdr::Endpoint,
    target: herdr::Target,
    /// `agent_session` as Herdr reports it for the pane now.
    reported: Value,
}

/// The Pi endpoint and pane as they are now. Guard: never the owner's HOME,
/// runtime directory or Herdr, only the script's ac-e2e-pi.* root.
fn arrangement() -> Arrangement {
    let root = PathBuf::from(var("AC_E2E_ROOT"));
    assert!(
        root.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("ac-e2e-pi.")),
        "{}",
        root.display()
    );
    let runtime = PathBuf::from(var("AC_E2E_RUNTIME"));
    let socket = var("AC_E2E_SOCKET");
    for p in [
        Path::new(&var("AC_E2E_HOME")),
        &runtime,
        Path::new(&socket),
        Path::new(&var("AC_E2E_STUB_LOG")),
    ] {
        assert!(
            p.starts_with(&root),
            "{} outside {}",
            p.display(),
            root.display()
        );
    }
    let mut endpoints = pi::discover(&runtime).unwrap();
    assert_eq!(endpoints.len(), 1, "one isolated Pi endpoint");
    let endpoint = endpoints.remove(0);
    assert!(Path::new(&endpoint.session_file).starts_with(&root));
    let server_pid: u32 = var("AC_E2E_SERVER_PID").parse().unwrap();
    let server = herdr::Endpoint {
        socket: socket.into(),
        server_pid,
        server_starttime: starttime(server_pid),
    };
    let got = raw("agent.get", json!({"target": var("AC_E2E_PANE")}));
    let agent = &got["result"]["agent"];
    println!(
        "e2e: agent.get status {} session {}",
        agent["agent_status"], agent["agent_session"]
    );
    let target = herdr::Target {
        pid: endpoint.pid,
        starttime: endpoint.process_starttime.parse().unwrap(),
        adapter: "pi".into(),
        workspace_id: agent["workspace_id"].as_str().unwrap().into(),
        tab_id: agent["tab_id"].as_str().unwrap().into(),
        pane_id: var("AC_E2E_PANE"),
    };
    assert_eq!(target.workspace_id, var("AC_E2E_WORKSPACE"));
    assert_eq!(target.tab_id, var("AC_E2E_TAB"));
    Arrangement {
        runtime,
        endpoint,
        server,
        target,
        reported: agent["agent_session"].clone(),
    }
}

/// Herdr's `kind:"path"` value is, byte for byte, the session file the
/// extension publishes in its manifest.
fn assert_reported_path(reported: &Value, session_file: &str) {
    assert_eq!(reported["kind"], "path", "{reported}");
    let value = reported["value"].as_str().unwrap();
    assert_eq!(value.as_bytes(), session_file.as_bytes(), "{reported}");
    println!(
        "e2e: herdr kind=path value == manifest session_file, {} bytes",
        value.len()
    );
}

/// Polls the operation on whichever endpoint the same process publishes now
/// until it leaves accepted/running (at most `secs`).
fn settle(a: &Arrangement, op: &str, secs: u64) -> (Progress, pi::PickupEvidence) {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut last = None;
    loop {
        match pi::pickup_status(
            &a.runtime,
            a.endpoint.pid,
            &a.endpoint.process_starttime,
            op,
            TIMEOUT,
        ) {
            Ok((p, e)) => {
                let now = (p.state, p.stage, p.code);
                if last != Some(now) {
                    println!("e2e: {:?} {:?} {}", p.state, p.stage, p.code);
                    last = Some(now);
                }
                if !matches!(p.state, State::Accepted | State::Running) || Instant::now() >= end {
                    return (p, e);
                }
            }
            // Between teardown and rebind the endpoint is briefly absent.
            Err(pi::Error::Identity | pi::Error::Io(_)) => {}
            Err(e) => panic!("status: {e:?}"),
        }
        assert!(Instant::now() < end, "{op} never reported");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "needs scripts/e2e-pi-herdr.sh (real private Pi + isolated Herdr)"]
fn real_pi_in_isolated_herdr_reaches_pickup_loaded() {
    let a = arrangement();
    assert_reported_path(&a.reported, &a.endpoint.session_file);
    let old_file = PathBuf::from(&a.endpoint.session_file);
    let old_bytes = fs::read(&old_file).unwrap();
    let requests = stub_log().len();

    let (request, armed) =
        pi::handoff_pickup(&a.endpoint, &a.server, Some(&a.target), OP.into(), TIMEOUT)
            .unwrap_or_else(|e| panic!("handoff_pickup: {e:?}"));
    println!("e2e: armed {:?} {}", armed.state, armed.code);
    assert_eq!(request.operation_id, OP);
    let (p, e) = settle(&a, OP, 120);
    println!(
        "e2e: old {} new {:?} handoff {:?} evidence {e:?}",
        p.old_session.session_id,
        p.new_session_id,
        p.handoff.as_ref().map(|h| &h.path)
    );
    assert_eq!(
        (p.state, p.stage, p.code),
        (State::Completed, Stage::PickupLoaded, "pickup_loaded"),
    );
    assert_eq!(p.old_session.session_id, a.endpoint.session_id);
    assert_eq!(p.old_session.session_file, old_file);
    let new_id = p.new_session_id.clone().unwrap();
    assert_ne!(new_id, a.endpoint.session_id);
    let new_file = e.new_session_file.clone().unwrap();
    assert_ne!(new_file, old_file);
    assert_eq!(e.parent_session.as_deref(), Some(old_file.as_path()));
    assert!(e.loaded_entry_id.is_some(), "{e:?}");
    let handoff = p.handoff.unwrap();
    assert!(handoff.path.starts_with(var("AC_E2E_ROOT")));
    assert_eq!(handoff.size, fs::metadata(&handoff.path).unwrap().len());

    // The old session is intact: same file, same bytes as before the click.
    assert_eq!(
        fs::read(&old_file).unwrap(),
        old_bytes,
        "old session changed"
    );
    // The command ran as a command: no model request carried it.
    let after = stub_log();
    assert!(
        after[requests..]
            .iter()
            .all(|r| r["pickup_command"] == false),
        "{after:?}"
    );

    // Herdr and the rotated manifest now name the new session, byte for byte.
    let now = arrangement();
    assert_eq!(now.endpoint.pid, a.endpoint.pid);
    assert_eq!(now.endpoint.session_id, new_id);
    assert_eq!(
        now.endpoint.session_file.as_bytes(),
        new_file.as_os_str().as_bytes()
    );
    let end = Instant::now() + Duration::from_secs(10);
    let mut reported = now.reported;
    while reported["value"].as_str() != Some(now.endpoint.session_file.as_str())
        && Instant::now() < end
    {
        std::thread::sleep(Duration::from_millis(200));
        reported = raw("agent.get", json!({"target": var("AC_E2E_PANE")}))["result"]["agent"]
            ["agent_session"]
            .clone();
    }
    assert_reported_path(&reported, &now.endpoint.session_file);
}

/// The arrangement once a click would reach the arm, so a test cannot pass
/// with nothing clicked. A cancelled or expired arm leaves the manifest
/// `busy` until the next Pi event (see
/// `real_pi_pickup_cancelled_arm_releases_busy`); if that is all that stands
/// in the way, one plain turn refreshes it, as the owner's next turn would.
fn available() -> Arrangement {
    let started = Instant::now();
    let mut refreshed = false;
    loop {
        let a = arrangement();
        let Some(reason) = pi::pickup_unavailable(&a.endpoint, Some(&a.target)) else {
            println!(
                "e2e: pickup available after {} ms (refresh turn: {refreshed})",
                started.elapsed().as_millis()
            );
            return a;
        };
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "pickup still unavailable: {reason}"
        );
        if reason == "busy" && !refreshed && started.elapsed() > Duration::from_secs(3) {
            let r = raw(
                "agent.prompt",
                json!({"target": var("AC_E2E_PANE"), "text": "e2e refresh"}),
            );
            assert_eq!(r["result"]["type"], "agent_prompted", "{r}");
            refreshed = true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Types `text` into the Pi editor without submitting it, then clicks pickup.
/// Whatever the outcome, the click must not turn the editor's content plus
/// the command into a prompt to the model.
fn click_with_editor_content(op: &str, text: &str) {
    let a = available();
    assert_reported_path(&a.reported, &a.endpoint.session_file);
    let requests = stub_log().len();
    let typed = raw(
        "pane.send_text",
        json!({"pane_id": var("AC_E2E_PANE"), "text": text}),
    );
    assert!(typed.get("error").is_none(), "{typed}");
    std::thread::sleep(Duration::from_millis(700));
    let r = pi::handoff_pickup(&a.endpoint, &a.server, Some(&a.target), op.into(), TIMEOUT);
    let armed = match &r {
        Ok((_, armed)) => format!("armed {:?} {}", armed.state, armed.code),
        Err(PickupError::Refused(reply)) => format!("refused {}", reply.code),
        Err(e) => format!("{e:?}"),
    };
    println!("e2e: click with editor content {text:?}: {armed}");
    assert!(
        !matches!(r, Err(PickupError::Unavailable(_))),
        "the click never reached the arm: {armed}"
    );
    // Give a diverted prompt time to reach the stub.
    std::thread::sleep(Duration::from_secs(4));
    let after = stub_log();
    let diverted: Vec<&Value> = after[requests..]
        .iter()
        .filter(|r| r["pickup_command"] == true)
        .collect();
    let tail: String = screen()
        .lines()
        .rev()
        .take(14)
        .collect::<Vec<_>>()
        .join(" | ");
    println!(
        "e2e: model requests after the click: {:?}",
        &after[requests..]
    );
    println!("e2e: screen (bottom up): {tail}");
    // Leave the pane as found for the next test: cancel a pending arm (a
    // diverted command never consumes it) and clear an unsent draft.
    if let Ok((request, _)) = &r {
        let now = arrangement();
        let c = pi::exchange(&now.endpoint, &request.cancel(), TIMEOUT);
        println!("e2e: cancel {:?}", c.map(|c| (c.state, c.code)));
    }
    raw(
        "pane.send_keys",
        json!({"pane_id": var("AC_E2E_PANE"), "keys": ["ctrl+u"]}),
    );
    assert!(
        diverted.is_empty(),
        "the pickup command reached the model as a prompt: {diverted:?}"
    );
}

#[test]
#[ignore = "needs scripts/e2e-pi-herdr.sh (real private Pi + isolated Herdr)"]
fn real_pi_pickup_with_a_draft_in_the_editor_is_not_diverted() {
    click_with_editor_content(DRAFT_OP, DRAFT);
}

#[test]
#[ignore = "needs scripts/e2e-pi-herdr.sh (real private Pi + isolated Herdr)"]
fn real_pi_pickup_with_slash_autocomplete_open_is_not_diverted() {
    click_with_editor_content(SLASH_OP, "/");
}

/// Arm (no click) and cancel on an idle Pi: the cancel must publish the
/// handoff capability again, or the client shows pickup `busy` until the
/// owner's next turn.
#[test]
#[ignore = "needs scripts/e2e-pi-herdr.sh (real private Pi + isolated Herdr)"]
fn real_pi_pickup_cancelled_arm_releases_busy() {
    let a = available();
    let mut request = a
        .endpoint
        .request(CANCEL_OP.into(), pi::Action::HandoffPickup)
        .unwrap();
    request.protocol_version = 3;
    let armed = pi::exchange(&a.endpoint, &request, TIMEOUT).unwrap();
    assert_eq!(
        (armed.state, armed.code.as_str()),
        (State::Accepted, "armed")
    );
    let cancelled = pi::exchange(&a.endpoint, &request.cancel(), TIMEOUT).unwrap();
    println!("e2e: cancel {:?} {}", cancelled.state, cancelled.code);
    assert_eq!(cancelled.state, State::Cancelled);
    let end = Instant::now() + Duration::from_secs(5);
    let reason = loop {
        let now = arrangement();
        let reason = pi::pickup_unavailable(&now.endpoint, Some(&now.target));
        if reason.is_none() || Instant::now() >= end {
            break reason;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    assert_eq!(
        reason, None,
        "manifest still unavailable 5 s after the cancel"
    );
}

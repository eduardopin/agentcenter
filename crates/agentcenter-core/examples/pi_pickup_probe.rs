//! Test driver only: one handoff-pickup click through the real Rust client
//! (`pi::handoff_pickup`: socket arm + revalidated Herdr `agent.prompt`), then
//! `pi::pickup_status` until a terminal state. Paths, IDs and codes only; never
//! handoff or session content. Run against isolated runtimes, never the owner's.
use agentcenter_core::control::{
    herdr,
    pi::{self, State},
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn starttime(pid: u32) -> Result<u64, String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
    agentcenter_core::procfs::stat::parse_stat(&text)
        .map(|s| s.starttime)
        .map_err(|e| format!("{e:?}"))
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 8 {
        return Err(
            "usage: pi_pickup_probe ISOLATED_RUNTIME HERDR_SOCKET HERDR_SERVER_PID \
                    WORKSPACE TAB PANE OPERATION_UUID"
                .into(),
        );
    }
    let runtime = Path::new(&args[1]);
    let mut endpoints = pi::discover(runtime).map_err(|e| format!("discovery: {e:?}"))?;
    if endpoints.len() != 1 {
        return Err(format!(
            "expected one isolated endpoint, got {}",
            endpoints.len()
        ));
    }
    let endpoint = endpoints.remove(0);
    let server_pid: u32 = args[3].parse().map_err(|_| "server pid")?;
    let server = herdr::Endpoint {
        socket: PathBuf::from(&args[2]),
        server_pid,
        server_starttime: starttime(server_pid)?,
    };
    let target = herdr::Target {
        pid: endpoint.pid,
        starttime: endpoint
            .process_starttime
            .parse()
            .map_err(|_| "starttime")?,
        adapter: "pi".into(),
        workspace_id: args[4].clone(),
        tab_id: args[5].clone(),
        pane_id: args[6].clone(),
    };
    let operation = args[7].clone();
    let timeout = Duration::from_secs(5);
    let (request, armed) = pi::handoff_pickup(
        &endpoint,
        &server,
        Some(&target),
        operation.clone(),
        timeout,
    )
    .map_err(|e| format!("handoff_pickup: {e:?}"))?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut polls = 0;
    let (progress, evidence) = loop {
        if Instant::now() >= deadline {
            return Err("status deadline exceeded".into());
        }
        std::thread::sleep(Duration::from_millis(50));
        polls += 1;
        // Same operation ID on whichever endpoint the same process publishes now.
        match pi::pickup_status(
            runtime,
            endpoint.pid,
            &endpoint.process_starttime,
            &operation,
            timeout,
        ) {
            Ok((p, e)) if !matches!(p.state, State::Accepted | State::Running) => break (p, e),
            Ok(_) => {}
            // Between teardown and rebind the endpoint is briefly absent.
            Err(pi::Error::Identity | pi::Error::Io(_)) => {}
            Err(e) => return Err(format!("status: {e:?}")),
        }
    };
    println!(
        "{}",
        serde_json::json!({
            "operation_id": request.operation_id, "armed": armed.code, "polls": polls,
            "state": progress.state, "code": progress.code, "stage": progress.stage,
            "old_session_id": progress.old_session.session_id,
            "old_session_file": progress.old_session.session_file,
            "new_session_id": progress.new_session_id,
            "handoff": progress.handoff.as_ref().map(|h| &h.path),
            "evidence": evidence,
        })
    );
    Ok(())
}

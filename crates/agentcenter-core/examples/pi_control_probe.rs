//! Test driver only: real discovery and socket exchange, never session content.
use agentcenter_core::control::pi::{self, Action, State};
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: pi_control_probe ISOLATED_RUNTIME ACTION OPERATION_UUID".into());
    }
    let action = match args[2].as_str() {
        "handoff" => Action::Handoff,
        "compact" => Action::Compact,
        _ => return Err("unsupported test action".into()),
    };
    let mut endpoints =
        pi::discover(Path::new(&args[1])).map_err(|e| format!("discovery: {e:?}"))?;
    if endpoints.len() != 1 {
        return Err(format!(
            "expected one isolated endpoint, got {}",
            endpoints.len()
        ));
    }
    let endpoint = endpoints.remove(0);
    let request = endpoint
        .request(args[3].clone(), action)
        .map_err(|e| format!("request: {e:?}"))?;
    let before = request.expected.clone();
    let mut reply = pi::exchange(&endpoint, &request, Duration::from_secs(2))
        .map_err(|e| format!("exchange: {e:?}"))?;
    let accepted = reply.state == State::Accepted;
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut polls = 0;
    while matches!(
        reply.state,
        State::Accepted | State::Running | State::Unknown
    ) {
        if Instant::now() >= deadline {
            return Err("status deadline exceeded".into());
        }
        std::thread::sleep(Duration::from_millis(5));
        // Retain the exact UUID and expected snapshot. No redispatch of the action.
        reply = pi::exchange(&endpoint, &request.status(), Duration::from_secs(2))
            .map_err(|e| format!("status: {e:?}"))?;
        polls += 1;
    }
    if reply.state != State::Completed {
        return Err(format!("terminal {:?}: {}", reply.state, reply.code));
    }
    let compaction = reply.compaction.as_ref().map(|c| {
        serde_json::json!({
            "capability": c.capability, "entryId": c.entry_id, "previousLeafId": c.previous_leaf_id
        })
    });
    println!(
        "{}",
        serde_json::json!({
            "operation_id": reply.operation_id, "accepted": accepted, "polls": polls,
            "state": "completed", "code": reply.code, "artifact_path": reply.artifact_path,
            "expected_before": before, "expected_after": reply.metadata.and_then(|m| m.expected),
            "compaction": compaction
        })
    );
    Ok(())
}

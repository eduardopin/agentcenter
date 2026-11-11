//! Test driver only: one step of the Pi control wire through the real Rust
//! client (discovery, request, exchange, Reply parsing and validation).
//! Prints what the client returned (state, code) or the client error; never
//! session content. Used by crates/agentcenter/tests/pi-controls-negatives.test.mjs.
use agentcenter_core::control::pi::{self, Action, Reply, State};
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn show(reply: &Reply, polls: u32) -> String {
    serde_json::json!({
        "state": format!("{:?}", reply.state).to_lowercase(), "code": reply.code,
        "has_artifact": reply.artifact_path.is_some(), "terminal": reply.state.is_terminal(),
        "operation_id": reply.operation_id, "polls": polls
    })
    .to_string()
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: pi_control_step ISOLATED_RUNTIME ACTION OPERATION_UUID".into());
    }
    let mut endpoints =
        pi::discover(Path::new(&args[1])).map_err(|e| format!("discovery: {e:?}"))?;
    if endpoints.len() != 1 {
        return Err(format!(
            "expected one isolated endpoint, got {}",
            endpoints.len()
        ));
    }
    let endpoint = endpoints.remove(0);
    // The dispatch action of the retained request; status/cancel/poll derive from it.
    let dispatch = |a: Action| {
        endpoint
            .request(args[3].clone(), a)
            .map_err(|e| format!("request: {e:?}"))
    };
    let timeout = Duration::from_secs(2);
    let out = match args[2].as_str() {
        "handoff" | "compact" => {
            let a = if args[2] == "handoff" {
                Action::Handoff
            } else {
                Action::Compact
            };
            pi::exchange(&endpoint, &dispatch(a)?, timeout)
        }
        "status" => pi::exchange(&endpoint, &dispatch(Action::Handoff)?.status(), timeout),
        "cancel" => pi::exchange(&endpoint, &dispatch(Action::Handoff)?.cancel(), timeout),
        "poll" => {
            let status = dispatch(Action::Handoff)?.status();
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut polls = 0;
            loop {
                let reply = pi::exchange(&endpoint, &status, timeout);
                polls += 1;
                match &reply {
                    Ok(r)
                        if matches!(r.state, State::Accepted | State::Running | State::Unknown) => {
                    }
                    other => {
                        match other {
                            Ok(r) => println!("{}", show(r, polls)),
                            Err(e) => {
                                println!("{}", serde_json::json!({ "error": format!("{e:?}") }))
                            }
                        }
                        return Ok(());
                    }
                }
                if Instant::now() >= deadline {
                    return Err("poll deadline exceeded".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        _ => return Err("unsupported test action".into()),
    };
    match out {
        Ok(r) => println!("{}", show(&r, 0)),
        Err(e) => println!("{}", serde_json::json!({ "error": format!("{e:?}") })),
    }
    Ok(())
}

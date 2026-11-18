//! Without an endpoint (no --llm, no AGENTCENTER_LLM, no setting) no socket is opened. The
//! counter is process-wide, so this binary holds one test only.

mod slm_support;

use agentcenter_core::slm::{Endpoint, Guard, Job, Message, Source, connections_opened, run};
use slm_support::fake;
use std::time::Duration;

fn job() -> Job {
    Job::new(
        vec![Message {
            role: "user".into(),
            content: "x".into(),
        }],
        Duration::from_secs(5),
    )
}

#[test]
fn no_socket_is_opened_unless_an_endpoint_is_given() {
    // Counter starts at zero in a fresh process; nothing above this line may connect.
    assert_eq!(connections_opened(), 0);
    // Off: no endpoint, so the job never reaches the network layer.
    let out = run(None, &job(), &Guard::labels(&["ok"]), "r");
    assert_eq!(out.source, Source::Rules);
    assert_eq!(connections_opened(), 0);
    // A refused endpoint never gets as far as a connection either.
    assert!(Endpoint::parse("http://10.0.0.5:8080").is_err());
    assert_eq!(connections_opened(), 0);
    // On: exactly one connection per job.
    let (ep, _srv) = fake("ok", Duration::ZERO);
    run(Some(&ep), &job(), &Guard::labels(&["ok"]), "r");
    assert_eq!(connections_opened(), 1);
}

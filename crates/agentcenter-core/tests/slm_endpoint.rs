//! Endpoint (loopback only) and the Job timeout.

mod slm_support;

use agentcenter_core::slm::{Endpoint, Guard, Job, Message, Source, run};
use slm_support::fake;
use std::time::{Duration, Instant};

fn job(timeout: Duration) -> Job {
    Job::new(
        vec![Message {
            role: "user".into(),
            content: "x".into(),
        }],
        timeout,
    )
}

#[test]
fn loopback_forms_are_accepted_and_everything_else_is_refused() {
    for ok in [
        "http://127.0.0.1:8080",
        "http://localhost:8080",
        "http://[::1]:8080",
        "http://127.0.0.1:8080/v1",
    ] {
        assert!(Endpoint::parse(ok).is_ok(), "{ok}");
    }
    for bad in [
        "http://10.0.0.5:8080",
        "http://0.0.0.0:8080",
        "http://example.com:8080",
        "https://127.0.0.1:8080",
        "http://127.0.0.1.evil.com:8080",
        "http://127.0.0.1:8080@10.0.0.5:80",
        "http://user@127.0.0.1:8080",
        "http://[::2]:8080",
        "http://127.0.0.1",
        "",
    ] {
        assert!(Endpoint::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_server_slower_than_the_timeout_yields_the_rules_within_timeout_plus_100_ms() {
    let (ep, _srv) = fake("ok", Duration::from_secs(3));
    let timeout = Duration::from_millis(300);
    let t = Instant::now();
    let out = run(Some(&ep), &job(timeout), &Guard::labels(&["ok"]), "rules");
    let took = t.elapsed();
    assert!(took < timeout + Duration::from_millis(100), "{took:?}");
    assert_eq!(out.source, Source::Rules);
    assert_eq!(out.text, "rules");
    assert_eq!(out.refused.as_deref(), Some("timed out"));
}

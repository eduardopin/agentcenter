//! Guard (label and schema check with a refusal reason) and the source tag, against a fake
//! in-process server. The grammar names the allowed labels; the guard enforces them anyway.

mod slm_support;

use agentcenter_core::slm::{Guard, Job, Message, Source, Verdict, run};
use slm_support::fake;
use std::time::Duration;

const LABELS: [&str; 3] = ["ok", "warn", "stuck"];
const GRAMMAR: &str = r#"root ::= "ok" | "warn" | "stuck""#;

fn job(timeout: Duration) -> Job {
    let mut j = Job::new(
        vec![Message {
            role: "user".into(),
            content: "classify".into(),
        }],
        timeout,
    );
    j.grammar = Some(GRAMMAR.into());
    j
}

fn refused(v: Verdict) -> String {
    match v {
        Verdict::Refused(r) => r.reason,
        Verdict::Accepted(t) => panic!("accepted {t:?}"),
    }
}

#[test]
fn a_label_outside_the_grammar_is_refused_and_the_output_carries_rules() {
    let (ep, _srv) = fake("catastrophe", Duration::ZERO);
    let out = run(
        Some(&ep),
        &job(Duration::from_secs(5)),
        &Guard::labels(&LABELS),
        "rules text",
    );
    assert_eq!(out.source, Source::Rules);
    assert_eq!(out.text, "rules text");
    assert_eq!(out.refused.as_deref(), Some("label_not_allowed"));
}

#[test]
fn an_allowed_label_is_the_model_output() {
    let (ep, _srv) = fake("warn\n", Duration::ZERO);
    let out = run(
        Some(&ep),
        &job(Duration::from_secs(5)),
        &Guard::labels(&LABELS),
        "rules text",
    );
    assert_eq!(out.source, Source::Model);
    assert_eq!(out.text, "warn");
    assert_eq!(out.refused, None);
}

#[test]
fn without_an_endpoint_the_output_is_the_rules_and_nothing_was_refused() {
    let out = run(
        None,
        &job(Duration::from_secs(5)),
        &Guard::labels(&LABELS),
        "rules text",
    );
    assert_eq!(out.source, Source::Rules);
    assert_eq!(out.text, "rules text");
    assert_eq!(out.refused, None);
}

#[test]
fn the_grammar_and_max_tokens_travel_in_the_request() {
    let (ep, srv) = fake("ok", Duration::ZERO);
    let mut j = job(Duration::from_secs(5));
    j.max_tokens = 7;
    run(Some(&ep), &j, &Guard::labels(&LABELS), "r");
    let req = srv.join().unwrap();
    assert!(req.contains("\"grammar\""), "{req}");
    assert!(req.contains("\"max_tokens\":7"), "{req}");
}

#[test]
fn label_guard_cases() {
    let g = Guard::labels(&LABELS);
    assert_eq!(g.check(" stuck \n"), Verdict::Accepted("stuck".into()));
    assert_eq!(refused(g.check("")), "empty");
    assert_eq!(refused(g.check("   ")), "empty");
    assert_eq!(refused(g.check("OK")), "label_not_allowed");
    assert_eq!(refused(g.check("ok because")), "label_not_allowed");
    assert_eq!(refused(g.check("ok\nwarn")), "label_not_allowed");
}

#[test]
fn json_label_guard_cases() {
    let g = Guard::json_label("state", &LABELS);
    assert_eq!(
        g.check(r#"{"state": "warn"}"#),
        Verdict::Accepted("warn".into())
    );
    assert_eq!(refused(g.check("warn")), "not_json");
    assert_eq!(refused(g.check(r#"{"other": "warn"}"#)), "missing_field");
    assert_eq!(refused(g.check(r#"{"state": 3}"#)), "missing_field");
    assert_eq!(refused(g.check(r#"{"state": "boom"}"#)), "label_not_allowed");
    assert_eq!(refused(g.check("")), "empty");
}

#[test]
fn a_refusal_names_a_reason_that_is_a_short_token() {
    let g = Guard::labels(&LABELS);
    let reason = refused(g.check("SECRET-TRANSCRIPT-LINE that must not be echoed"));
    // The reason never quotes the model output.
    assert_eq!(reason, "label_not_allowed");
}

#[test]
fn source_serializes_in_lowercase() {
    assert_eq!(serde_json::to_string(&Source::Rules).unwrap(), "\"rules\"");
    assert_eq!(serde_json::to_string(&Source::Model).unwrap(), "\"model\"");
}

//! Briefing opcional do modelo: guard de saida, endpoint so de loopback e cliente HTTP.
//! O servidor falso roda em 127.0.0.1:0 dentro do teste; nada sai da maquina.

use agentcenter_core::briefing::{
    Briefing, ClientError, Endpoint, Message, Rejection, Source, ask, brief, guard, messages,
};
use agentcenter_core::insights::{Finding, facts, fallback};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

fn finding(id: &str, severity: &str, text: &str, must: &[&str]) -> Finding {
    Finding {
        id: id.into(),
        severity: severity.into(),
        score: 50,
        text: text.into(),
        must: must.iter().map(|s| s.to_string()).collect(),
    }
}

fn context_full() -> Finding {
    finding(
        "context_full",
        "high",
        "3 sessions are past 75% of the context window; the fullest is api (pid 4411) at 91%. \
         Action: write a handoff and start a fresh session.",
        &["91%", "handoff"],
    )
}

fn idle_memory() -> Finding {
    finding(
        "idle_memory",
        "low",
        "12 idle sessions hold 2.6 GB of the 9.8 GB used by agents. Action: agentcenter-cli reclaim.",
        &["2.6", "reclaim"],
    )
}

fn pair() -> Vec<Finding> {
    vec![context_full(), idle_memory()]
}

const GOOD: &str = "Three sessions are past 75% of their context window, and api (pid 4411) is at 91%. \
    Write a handoff for it and start a fresh session, then free 2.6 GB with agentcenter-cli reclaim.";

fn reject(text: &str, fs: &[Finding]) -> String {
    match guard(text, fs) {
        Ok(t) => panic!("expected a rejection, got: {t}"),
        Err(Rejection { reason }) => reason,
    }
}

// ------------------------------------------------------- review findings

#[test]
fn control_characters_in_the_answer_are_refused() {
    let fs = pair();
    for bad in [
        GOOD.replace("Write a handoff", "\x1b[2JWrite a handoff"),
        GOOD.replace("Write a handoff", "\x1b]0;title\x07Write a handoff"),
        GOOD.replace("Write", "Write\u{0}"),
    ] {
        assert_eq!(reject(&bad, &fs), "control character");
    }
}

#[test]
fn fullwidth_digits_are_numbers_too() {
    let fs = pair();
    let bad = GOOD.replace("2.6 GB", "\u{FF19}\u{FF19} GB");
    assert!(
        reject(&bad, &fs).starts_with("number not in findings"),
        "{bad}"
    );
    // a numeric character with no decimal value is refused as well
    let bad = GOOD.replace("2.6 GB", "\u{00BD} GB");
    assert!(
        reject(&bad, &fs).starts_with("number not in findings"),
        "{bad}"
    );
    // fullwidth forms of an allowed number are fine
    let ok = GOOD.replace("pid 4411", "pid \u{FF14}\u{FF14}\u{FF11}\u{FF11}");
    assert!(guard(&ok, &fs).is_ok());
}

#[test]
fn spelled_numbers_above_twelve_are_refused_unless_in_the_findings() {
    let fs = pair();
    for w in [
        "twenty", "nineteen", "hundred", "thousand", "million", "thirty",
    ] {
        let bad = GOOD.replace("Three sessions", &format!("Three sessions, {w} sessions"));
        assert!(
            reject(&bad, &fs).starts_with("number not in findings"),
            "{w}"
        );
    }
    let mut fs2 = pair();
    fs2[1].text = "Twenty idle sessions hold 2.6 GB. Action: agentcenter-cli reclaim.".into();
    let ok = GOOD.replace("Three sessions", "Three sessions, twenty sessions");
    assert!(guard(&ok, &fs2).is_ok());
}

#[test]
fn spelled_numbers_up_to_twelve_must_match_the_findings() {
    let fs = pair();
    // the findings say 3 sessions, 12 idle sessions, 75%, 91%, 2.6 GB
    for w in [
        "Nine", "Four", "Five", "Six", "Seven", "Eight", "Ten", "Eleven", "Zero",
    ] {
        let bad = GOOD.replace("Three sessions", &format!("{w} sessions"));
        assert!(
            reject(&bad, &fs).starts_with("number not in findings"),
            "{w}"
        );
    }
    for w in ["a dozen", "dozens of", "a billion"] {
        let bad = GOOD.replace("Three sessions", &format!("Three sessions, {w} sessions"));
        assert!(
            reject(&bad, &fs).starts_with("number not in findings"),
            "{w}"
        );
    }
    // a spelled number whose digits are in the findings passes
    let ok = GOOD.replace("Three sessions", "Three sessions, twelve of them idle,");
    assert!(guard(&ok, &fs).is_ok());
    // "one" and "two" are ordinary prose, as 1 and 2 are always allowed
    let ok = GOOD.replace("Write a handoff for it", "Write one or two handoffs for it");
    assert!(guard(&ok, &fs).is_ok());
    // a word in the findings' own text passes
    let mut fs2 = pair();
    fs2[1].text = "A dozen idle sessions hold 2.6 GB. Action: agentcenter-cli reclaim.".into();
    let ok = GOOD.replace("Three sessions", "Three sessions, a dozen idle,");
    assert!(guard(&ok, &fs2).is_ok());
}

#[test]
fn invisible_format_characters_are_refused() {
    let fs = pair();
    for c in [
        '\u{200B}',
        '\u{200D}',
        '\u{200F}',
        '\u{202A}',
        '\u{202E}',
        '\u{2060}',
        '\u{2066}',
        '\u{2069}',
        '\u{FEFF}',
        '\u{00AD}',
        '\u{061C}',
        '\u{E0041}',
    ] {
        let bad = GOOD.replace("Write a handoff", &format!("Write{c} a handoff"));
        assert_eq!(reject(&bad, &fs), "control character", "U+{:04X}", c as u32);
    }
    // a byte order mark at the very start is refused too
    assert_eq!(reject(&format!("\u{FEFF}{GOOD}"), &fs), "control character");
}

#[test]
fn think_blocks_are_discarded_before_the_answer() {
    let fs = pair();
    let raw =
        format!("<think>\nLet me work out the answer.\n\nMaybe 77 things.\n</think>\n\n{GOOD}");
    assert_eq!(guard(&raw, &fs).unwrap(), GOOD);
    // an unclosed block swallows the rest, leaving nothing
    let raw = format!("<think>hmm {GOOD}");
    assert_eq!(reject(&raw, &fs), "empty");
}

// ---------------------------------------------------------------- messages

#[test]
fn messages_are_the_four_reference_turns() {
    let fs = pair();
    let m = messages(&fs);
    let roles: Vec<&str> = m.iter().map(|x| x.role.as_str()).collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);
    assert!(
        m[0].content
            .starts_with("You brief a developer who runs many AI coding agents")
    );
    assert!(m[1].content.contains("api (pid 4411) at 91%"));
    assert!(m[2].content.starts_with("Three sessions are past 75%"));
    assert_eq!(m[3].content, facts(&fs));
    let v = serde_json::to_value(&m[3]).unwrap();
    assert_eq!(v, json!({"role": "user", "content": facts(&fs)}));
    let _: &Message = &m[0];
}

// ------------------------------------------------------------------- guard

#[test]
fn guard_accepts_clean_lines() {
    let fs = pair();
    let mut over = String::from("Write a handoff for api at 91%. ");
    while over.chars().count() < 420 {
        over.push('a');
    }
    assert_eq!(over.chars().count(), 420);
    let lines: Vec<(&str, Vec<Finding>)> = vec![
        (GOOD, fs.clone()),
        (
            "api (pid 4411) is at 91% of its context window. Write a handoff now.",
            fs.clone(),
        ),
        ("Write a handoff 91%.", fs.clone()),
        (&over, fs.clone()),
        (
            "Do 1 thing first: write a handoff for api (pid 4411) at 91%.",
            fs.clone(),
        ),
        (
            "Run agentcenter-cli reclaim to free 2.60 GB, the idle sessions hold it.",
            vec![idle_memory()],
        ),
        (
            "Hand off api at 91% with a handoff, then 2 more steps follow.",
            fs.clone(),
        ),
        (
            "Three sessions waste memory, so run agentcenter-cli reclaim now.",
            vec![finding(
                "x",
                "high",
                "3 sessions waste memory. Action: agentcenter-cli reclaim.",
                &["3", "reclaim"],
            )],
        ),
        (
            "Twelve idle sessions hold 2.6 GB; run agentcenter-cli reclaim.",
            vec![finding(
                "x",
                "low",
                "12 idle sessions hold 2.6 GB. Action: agentcenter-cli reclaim.",
                &["12", "reclaim"],
            )],
        ),
        ("API (PID 4411) IS AT 91%. WRITE A HANDOFF.", fs.clone()),
    ];
    assert!(lines.len() >= 8);
    for (line, set) in lines {
        let got = guard(line, &set).unwrap_or_else(|r| panic!("{line:?} rejected: {}", r.reason));
        assert_eq!(got, line.trim());
    }
}

#[test]
fn guard_strips_special_tokens_and_keeps_the_first_paragraph() {
    let fs = pair();
    let t = guard(&format!("<|im_start|>assistant {GOOD}<|im_end|>"), &fs).unwrap();
    assert_eq!(t, format!("assistant {GOOD}"));
    let t = guard(&format!("<think>  </think>\n{GOOD}"), &fs).unwrap();
    assert_eq!(t, GOOD);
    let t = guard(&format!("{GOOD}\n\nNote: this took 99 steps."), &fs).unwrap();
    assert_eq!(t, GOOD, "the invented 99 sits in the dropped paragraph");
    let t = guard("Three sessions are past 75% of their context window,\nand api (pid 4411)   is at 91%.\nWrite a handoff.", &fs).unwrap();
    assert_eq!(
        t,
        "Three sessions are past 75% of their context window, and api (pid 4411) is at 91%. Write a handoff."
    );
}

#[test]
fn guard_rejects_empty_and_short() {
    let fs = pair();
    assert_eq!(reject("", &fs), "empty");
    assert_eq!(reject("   \n ", &fs), "empty");
    assert_eq!(reject("<|im_end|>", &fs), "empty");
    assert_eq!(reject("<think></think>", &fs), "empty");
    assert_eq!(reject("Write a handoff 91%", &fs), "empty", "19 chars");
}

#[test]
fn guard_rejects_over_420_chars() {
    let fs = pair();
    let mut s = String::from("Write a handoff for api at 91%. ");
    while s.chars().count() < 421 {
        s.push('a');
    }
    assert_eq!(reject(&s, &fs), "too long");
}

#[test]
fn guard_rejects_out_of_role_phrases() {
    let fs = pair();
    let r = reject("As an AI, I would write a handoff for api at 91%.", &fs);
    assert_eq!(r, "out of role: as an ai");
    let r = reject(
        "I cannot say, but api (pid 4411) is at 91% so write a handoff.",
        &fs,
    );
    assert_eq!(r, "out of role: i cannot");
    // eco do prompt de sistema
    let echo = "You brief a developer who runs many AI coding agents on one machine. Write 2 sentences in plain English.";
    assert_eq!(reject(echo, &fs), "out of role: plain english");
    let r = reject(
        "The findings say api (pid 4411) is at 91%; write a handoff.",
        &fs,
    );
    assert_eq!(r, "out of role: the findings");
    // OOC vem antes do numero inventado
    assert!(reject("As an AI I saw 93% and no handoff.", &fs).starts_with("out of role"));
}

#[test]
fn guard_rejects_a_number_not_in_the_findings() {
    let fs = pair();
    let r = reject(
        "Three sessions are past 75%, and api (pid 4411) is at 93%. Write a handoff.",
        &fs,
    );
    assert_eq!(r, "number not in findings: 93");
    let r = reject(
        "Write a handoff for api at 91%, then free 3.4 GB with agentcenter-cli reclaim.",
        &fs,
    );
    assert_eq!(r, "number not in findings: 3.4");
    // zeros finais so valem depois de um ponto decimal: 20 nao vira 2
    let r = reject(
        "Write a handoff for api at 91%, with 20 sessions to go.",
        &fs,
    );
    assert_eq!(r, "number not in findings: 20");
    // um ponto final depois do numero nao vira decimal
    assert!(guard("Write a handoff for api at 91%. Then wait.", &fs).is_ok());
}

#[test]
fn guard_rejects_a_missing_key_fact() {
    let fs = pair();
    let r = reject(
        "Three sessions are past 75% of their context window. Write a handoff for api now.",
        &fs,
    );
    assert_eq!(r, "missing key fact: 91%");
    let r = reject(
        "Api (pid 4411) is at 91% of its context window, so slow down and rest.",
        &fs,
    );
    assert_eq!(r, "missing key fact: handoff");
}

// ---------------------------------------------------------------- endpoint

#[test]
fn endpoint_accepts_loopback_only_forms() {
    for (url, port) in [
        ("http://127.0.0.1:8080", 8080),
        ("http://127.0.0.1:8080/", 8080),
        ("http://127.0.0.1:8080/v1", 8080),
        ("http://127.0.0.1:8080/v1/", 8080),
        ("http://localhost:18888", 18888),
        ("http://localhost:18888/v1", 18888),
        ("http://[::1]:9000", 9000),
        ("http://[::1]:9000/v1", 9000),
    ] {
        let e = Endpoint::parse(url).unwrap_or_else(|err| panic!("{url}: {err}"));
        assert!(
            e.addr().ip().is_loopback(),
            "{url} resolved to {}",
            e.addr()
        );
        assert_eq!(e.addr().port(), port, "{url}");
    }
}

#[test]
fn endpoint_refuses_everything_else() {
    for url in [
        "http://10.0.0.5:8080",
        "http://example.com",
        "http://example.com:80",
        "https://127.0.0.1:1",
        "http://127.0.0.1.evil.com:80",
        "http://user@127.0.0.1:80",
        "http://user:pw@localhost:80",
        "http://localhost.evil.com:80",
        "http://0.0.0.0:80",
        "http://127.0.0.2:80",
        "http://[::]:80",
        "http://127.0.0.1",
        "http://127.0.0.1:",
        "http://127.0.0.1:0",
        "http://127.0.0.1:99999",
        "http://127.0.0.1:+80",
        "http://127.0.0.1:80/v2",
        "http://127.0.0.1:80/v1/chat",
        "http://127.0.0.1:80/other",
        "http://127.0.0.1:80?x=1",
        "http://127.0.0.1:80/v1?x=1",
        "http://127.0.0.1:80#frag",
        "http://127.0.0.1:80\\@evil.com",
        "127.0.0.1:80",
        "ftp://127.0.0.1:80",
        "",
        " http://127.0.0.1:80",
        "HTTP://127.0.0.1:80x",
    ] {
        assert!(Endpoint::parse(url).is_err(), "{url:?} must be refused");
    }
}

// ----------------------------------------------------------- stub server

/// Le o pedido inteiro (cabecalho + corpo) e o devolve como texto.
fn read_request(s: &mut std::net::TcpStream) -> String {
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = s.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(h) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..h]).to_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map(|v| v.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            if buf.len() >= h + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Sobe um servidor de uma conexao; devolve o endpoint e o pedido recebido.
fn stub(reply: Vec<u8>) -> (Endpoint, JoinHandle<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let req = read_request(&mut s);
        let _ = s.write_all(&reply);
        let _ = s.flush();
        req
    });
    (
        Endpoint::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
        h,
    )
}

/// Servidor que aceita, le o pedido e nunca responde (segura a conexao por 3 s).
fn silent() -> Endpoint {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let _ = read_request(&mut s);
        thread::sleep(Duration::from_secs(3));
    });
    Endpoint::parse(&format!("http://127.0.0.1:{port}")).unwrap()
}

fn chat_body(content: &str) -> String {
    json!({"choices": [{"message": {"role": "assistant", "content": content}}]}).to_string()
}

fn http(status: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn chunked(body: &str, size: usize) -> Vec<u8> {
    let mut out = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
    for part in body.as_bytes().chunks(size) {
        out.extend_from_slice(format!("{:x};ext=1\r\n", part.len()).as_bytes());
        out.extend_from_slice(part);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"0\r\n\r\n");
    out
}

const T: Duration = Duration::from_secs(3);

// ------------------------------------------------------------------ client

#[test]
fn ask_posts_the_chat_request_and_returns_the_content() {
    let fs = pair();
    let (ep, srv) = stub(http("200 OK", &chat_body(GOOD)));
    let got = ask(&ep, &messages(&fs), T).unwrap();
    assert_eq!(got, GOOD);
    let req = srv.join().unwrap();
    let (head, body) = req.split_once("\r\n\r\n").unwrap();
    assert!(
        head.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"),
        "{head}"
    );
    let low = head.to_lowercase();
    assert!(low.contains("connection: close"));
    assert!(low.contains("content-type: application/json"));
    assert!(low.contains(&format!("host: {}", ep.addr())));
    let v: Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["model"], "local");
    assert_eq!(v["max_tokens"], 120);
    assert_eq!(v["temperature"], 0);
    assert_eq!(v["messages"].as_array().unwrap().len(), 4);
    assert_eq!(v["messages"][3]["content"], facts(&fs));
}

#[test]
fn ask_works_through_the_localhost_name() {
    let (ep, _srv) = stub(http("200 OK", &chat_body(GOOD)));
    let ep = Endpoint::parse(&format!("http://localhost:{}/v1", ep.addr().port())).unwrap();
    assert_eq!(ask(&ep, &messages(&pair()), T).unwrap(), GOOD);
}

#[test]
fn ask_decodes_a_chunked_body() {
    let (ep, _srv) = stub(chunked(&chat_body(GOOD), 17));
    assert_eq!(ask(&ep, &messages(&pair()), T).unwrap(), GOOD);
}

#[test]
fn ask_reports_a_non_200_status() {
    let (ep, _srv) = stub(http("500 Internal Server Error", "{\"error\":\"boom\"}"));
    assert_eq!(
        ask(&ep, &messages(&pair()), T),
        Err(ClientError::Status(500))
    );
}

#[test]
fn ask_times_out_on_a_server_that_never_answers() {
    let ep = silent();
    let t0 = Instant::now();
    let r = ask(&ep, &messages(&pair()), Duration::from_millis(300));
    assert_eq!(r, Err(ClientError::Timeout));
    assert!(
        t0.elapsed() < Duration::from_millis(1500),
        "took {:?}",
        t0.elapsed()
    );
}

#[test]
fn ask_times_out_on_a_server_that_drips_forever() {
    // um byte a cada 100 ms: o prazo e total, nao por leitura
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let _ = read_request(&mut s);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10000\r\n\r\n");
        for _ in 0..50 {
            if s.write_all(b"x").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    });
    let ep = Endpoint::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    let t0 = Instant::now();
    let r = ask(&ep, &messages(&pair()), Duration::from_millis(500));
    assert_eq!(r, Err(ClientError::Timeout));
    assert!(
        t0.elapsed() < Duration::from_millis(1500),
        "took {:?}",
        t0.elapsed()
    );
}

#[test]
fn ask_refuses_a_body_over_64_kib() {
    let big = "a".repeat(100_000);
    let (ep, _srv) = stub(http("200 OK", &chat_body(&big)));
    assert_eq!(ask(&ep, &messages(&pair()), T), Err(ClientError::TooLarge));
    let (ep, _srv) = stub(chunked(&chat_body(&big), 4096));
    assert_eq!(ask(&ep, &messages(&pair()), T), Err(ClientError::TooLarge));
}

#[test]
fn ask_refuses_garbage() {
    for reply in [
        http("200 OK", "this is not json"),
        http("200 OK", "{\"choices\": []}"),
        http("200 OK", "{\"choices\": [{\"message\": {\"content\": 5}}]}"),
        http("200 OK", ""),
        b"not http at all\r\n\r\n".to_vec(),
        Vec::new(),
    ] {
        let (ep, _srv) = stub(reply.clone());
        let r = ask(&ep, &messages(&pair()), T);
        assert!(
            matches!(r, Err(ClientError::Malformed(_)) | Err(ClientError::Io(_))),
            "{:?} -> {r:?}",
            String::from_utf8_lossy(&reply)
        );
    }
}

#[test]
fn ask_fails_fast_when_nothing_listens() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let ep = Endpoint::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    assert!(matches!(
        ask(&ep, &messages(&pair()), T),
        Err(ClientError::Io(_))
    ));
}

// ------------------------------------------------------------------- brief

#[test]
fn brief_without_endpoint_is_the_rule_text() {
    let fs = pair();
    let b = brief(&fs, None, T);
    assert_eq!(b.text, fallback(&fs));
    assert_eq!(b.source, Source::Rules);
    assert_eq!(b.rejected, None);
}

#[test]
fn brief_shows_a_passing_model_answer() {
    let fs = pair();
    let (ep, _srv) = stub(http("200 OK", &chat_body(&format!("{GOOD}\n"))));
    let b = brief(&fs, Some(&ep), T);
    assert_eq!(b.text, GOOD);
    assert_eq!(b.source, Source::Model);
    assert_eq!(b.rejected, None);
}

#[test]
fn brief_shows_a_chunked_model_answer() {
    let fs = pair();
    let (ep, _srv) = stub(chunked(&chat_body(GOOD), 9));
    let b = brief(&fs, Some(&ep), T);
    assert_eq!((b.text.as_str(), b.source), (GOOD, Source::Model));
}

#[test]
fn brief_falls_back_on_an_invented_number() {
    let fs = pair();
    let lie = "Three sessions are past 75%, and api (pid 4411) is at 93%. Write a handoff.";
    let (ep, _srv) = stub(http("200 OK", &chat_body(lie)));
    let b = brief(&fs, Some(&ep), T);
    assert_eq!(b.text, fallback(&fs));
    assert_eq!(b.source, Source::Rules);
    assert_eq!(b.rejected.as_deref(), Some("number not in findings: 93"));
}

#[test]
fn brief_falls_back_on_a_server_error() {
    let fs = pair();
    let (ep, _srv) = stub(http("500 Internal Server Error", "{}"));
    let b = brief(&fs, Some(&ep), T);
    assert_eq!(b.text, fallback(&fs));
    assert_eq!(b.source, Source::Rules);
    assert!(b.rejected.unwrap().contains("500"));
}

#[test]
fn brief_falls_back_on_a_timeout_within_the_limit() {
    let fs = pair();
    let ep = silent();
    let t0 = Instant::now();
    let b = brief(&fs, Some(&ep), Duration::from_millis(300));
    assert!(t0.elapsed() < Duration::from_millis(1500));
    assert_eq!(b.text, fallback(&fs));
    assert_eq!(b.source, Source::Rules);
    assert!(b.rejected.unwrap().contains("timed out"));
}

#[test]
fn brief_falls_back_on_garbage_and_on_oversized_bodies() {
    let fs = pair();
    let (ep, _srv) = stub(http("200 OK", "<html>nope</html>"));
    let b = brief(&fs, Some(&ep), T);
    assert_eq!(
        (b.text.as_str(), b.source),
        (fallback(&fs).as_str(), Source::Rules)
    );
    assert!(b.rejected.is_some());
    let (ep, _srv) = stub(http("200 OK", &chat_body(&"a".repeat(100_000))));
    let b = brief(&fs, Some(&ep), T);
    assert_eq!(
        (b.text.as_str(), b.source),
        (fallback(&fs).as_str(), Source::Rules)
    );
    assert!(b.rejected.unwrap().contains("too large"));
}

#[test]
fn brief_falls_back_when_nothing_listens() {
    let fs = pair();
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let ep = Endpoint::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    let b = brief(&fs, Some(&ep), T);
    assert_eq!(
        (b.text.as_str(), b.source),
        (fallback(&fs).as_str(), Source::Rules)
    );
    assert!(b.rejected.is_some());
}

#[test]
fn briefing_serializes_the_source_in_lowercase() {
    let rules = Briefing {
        text: "a".into(),
        source: Source::Rules,
        rejected: Some("why".into()),
    };
    assert_eq!(
        serde_json::to_value(&rules).unwrap(),
        json!({"text": "a", "source": "rules", "rejected": "why"})
    );
    let model = Briefing {
        text: "b".into(),
        source: Source::Model,
        rejected: None,
    };
    assert_eq!(
        serde_json::to_value(&model).unwrap(),
        json!({"text": "b", "source": "model", "rejected": null})
    );
}

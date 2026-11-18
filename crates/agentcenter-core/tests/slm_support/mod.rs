//! Fake in-process model server shared by the slm tests. No model, no network beyond 127.0.0.1:0.
#![allow(dead_code)]

use agentcenter_core::slm::Endpoint;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::{self, JoinHandle};
use std::time::Duration;

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

/// One-shot server answering with a chat completion carrying `content`, after `delay`.
/// The handle yields the request it received.
pub fn fake(content: &str, delay: Duration) -> (Endpoint, JoinHandle<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let body =
        json!({"choices": [{"message": {"role": "assistant", "content": content}}]}).to_string();
    let h = thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let req = read_request(&mut s);
        thread::sleep(delay);
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = s.write_all(reply.as_bytes());
        req
    });
    (
        Endpoint::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
        h,
    )
}

//! Cliente HTTP minimo (std) para `/v1/chat/completions`, so de loopback. Sem dependencia nova.

use super::endpoint::Endpoint;
use super::job::Job;
use serde_json::{Value, json};
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Teto do corpo da resposta do modelo.
const MAX_BODY: usize = 64 * 1024;
/// Teto do cabecalho HTTP da resposta.
const MAX_HEAD: usize = 16 * 1024;

/// Conexoes abertas por este processo; os testes leem para provar que sem endpoint ha zero.
static CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

/// Quantas conexoes TCP o cliente abriu neste processo.
pub fn connections_opened() -> usize {
    CONNECTIONS.load(Ordering::SeqCst)
}

/// Falha ao falar com o modelo.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientError {
    Timeout,
    Io(String),
    Status(u16),
    TooLarge,
    Malformed(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Timeout => f.write_str("timed out"),
            ClientError::Io(e) => write!(f, "io error: {e}"),
            ClientError::Status(s) => write!(f, "http status {s}"),
            ClientError::TooLarge => f.write_str("response too large"),
            ClientError::Malformed(e) => write!(f, "malformed response: {e}"),
        }
    }
}

impl std::error::Error for ClientError {}

fn io_err(e: std::io::Error) -> ClientError {
    match e.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => ClientError::Timeout,
        _ => ClientError::Io(e.to_string()),
    }
}

fn malformed(why: &str) -> ClientError {
    ClientError::Malformed(why.into())
}

/// O que falta do prazo total; zero ou menos e `Timeout`.
fn remaining(deadline: Instant) -> Result<Duration, ClientError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(ClientError::Timeout)
}

/// Decodifica um corpo chunked. `None` = ainda incompleto.
fn decode_chunked(raw: &[u8]) -> Result<Option<Vec<u8>>, ClientError> {
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        let Some(nl) = raw[i..].windows(2).position(|w| w == b"\r\n") else {
            return Ok(None);
        };
        let line = std::str::from_utf8(&raw[i..i + nl]).map_err(|_| malformed("chunk size"))?;
        let hex = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(hex, 16).map_err(|_| malformed("chunk size"))?;
        i += nl + 2;
        if size == 0 {
            return Ok(Some(out));
        }
        if out.len().saturating_add(size) > MAX_BODY {
            return Err(ClientError::TooLarge);
        }
        if raw.len() < i + size + 2 {
            return Ok(None);
        }
        out.extend_from_slice(&raw[i..i + size]);
        if &raw[i + size..i + size + 2] != b"\r\n" {
            return Err(malformed("chunk framing"));
        }
        i += size + 2;
    }
}

/// Tenta tirar o corpo do que ja chegou. `None` = falta ler; `eof` = o servidor fechou.
fn parse_response(buf: &[u8], eof: bool) -> Result<Option<Vec<u8>>, ClientError> {
    let Some(h) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        if eof {
            return Err(malformed("no http header"));
        }
        return if buf.len() > MAX_HEAD {
            Err(malformed("header too long"))
        } else {
            Ok(None)
        };
    };
    let head = std::str::from_utf8(&buf[..h]).map_err(|_| malformed("header is not text"))?;
    let body = &buf[h + 4..];
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .filter(|l| l.starts_with("HTTP/1."))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| malformed("status line"))?;
    if status != 200 {
        return Err(ClientError::Status(status));
    }
    let mut length: Option<usize> = None;
    let mut chunked = false;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        if k == "content-length" {
            let n = v
                .parse::<usize>()
                .map_err(|_| malformed("content-length"))?;
            if length.is_some_and(|old| old != n) {
                return Err(malformed("conflicting content-length"));
            }
            length = Some(n);
        } else if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        }
    }
    if chunked {
        return match decode_chunked(body)? {
            None if eof => Err(malformed("truncated chunked body")),
            done => Ok(done),
        };
    }
    match length {
        Some(n) if n > MAX_BODY => Err(ClientError::TooLarge),
        Some(n) if body.len() >= n => Ok(Some(body[..n].to_vec())),
        Some(_) if eof => Err(malformed("truncated body")),
        Some(_) => Ok(None),
        None if eof => Ok(Some(body.to_vec())),
        None => Ok(None),
    }
}

/// Le a resposta ate o corpo estar completo, dentro do prazo total.
fn read_body(s: &mut TcpStream, deadline: Instant) -> Result<Vec<u8>, ClientError> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let mut eof = false;
    loop {
        if let Some(body) = parse_response(&buf, eof)? {
            return Ok(body);
        }
        if buf.len() > MAX_HEAD + MAX_BODY {
            return Err(ClientError::TooLarge);
        }
        s.set_read_timeout(Some(remaining(deadline)?))
            .map_err(io_err)?;
        match s.read(&mut tmp) {
            Ok(0) => eof = true,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(io_err(e)),
        }
    }
}

/// POST `/v1/chat/completions` no servidor local e devolve `choices[0].message.content`.
/// `job.timeout` e o prazo total (conexao, escrita e leitura juntas).
pub fn complete(endpoint: &Endpoint, job: &Job) -> Result<String, ClientError> {
    let deadline = Instant::now() + job.timeout;
    let mut body = json!({
        "model": "local",
        "messages": job.prompt,
        "max_tokens": job.max_tokens,
        "temperature": 0,
    });
    if let Some(g) = &job.grammar {
        body["grammar"] = Value::String(g.clone());
    }
    let body = body.to_string();
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
         Accept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        endpoint.addr(),
        body.len()
    );
    let timeout = remaining(deadline)?;
    CONNECTIONS.fetch_add(1, Ordering::SeqCst);
    let mut s = TcpStream::connect_timeout(&endpoint.addr(), timeout).map_err(io_err)?;
    s.set_write_timeout(Some(remaining(deadline)?))
        .map_err(io_err)?;
    s.write_all(request.as_bytes()).map_err(io_err)?;
    let raw = read_body(&mut s, deadline)?;
    let v: Value =
        serde_json::from_slice(&raw).map_err(|e| ClientError::Malformed(e.to_string()))?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| malformed("no choices[0].message.content"))
}

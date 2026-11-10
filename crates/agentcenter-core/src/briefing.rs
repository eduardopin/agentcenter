//! Briefing opcional do modelo: as mensagens do chat, o guard da saida e um cliente HTTP
//! so de loopback. Os achados nunca saem da maquina: `Endpoint` nao aceita outro destino.
//! Sem dependencia nova (std + serde); sem regex, a varredura e escrita a mao.

use crate::insights::{Finding, facts, fallback};
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

const SYSTEM: &str = "You brief a developer who runs many AI coding agents on one machine. \
You get numbered findings, most urgent first. Write 2 sentences in plain English. \
Sentence 1: the most urgent problem, with its numbers. \
Sentence 2: what to do first, naming the command if one is given. \
Use only numbers that appear in the findings. No lists, no preamble.";

const EXAMPLE_IN: &str = "1. [high] 3 sessions are past 75% of their context window; the fullest is api (pid 4411) at 91%. \
Action: write a handoff and start a fresh session.\n\
2. [low] 12 idle sessions hold 2.6 GB. Action: agentcenter-cli reclaim.";

const EXAMPLE_OUT: &str = "Three sessions are past 75% of their context window, and api (pid 4411) is at 91%. \
Write a handoff for it and start a fresh session, then free 2.6 GB with agentcenter-cli reclaim.";

/// Frases de quem saiu do papel ou repetiu o prompt.
const OOC: [&str; 11] = [
    "as an ai",
    "language model",
    "i cannot",
    "i can't",
    "i'm unable",
    "instructions",
    "the findings",
    "sentence 1",
    "sentence 2",
    "numbered findings",
    "plain english",
];

const WORDS: [(&str, &str); 12] = [
    ("one", "1"),
    ("two", "2"),
    ("three", "3"),
    ("four", "4"),
    ("five", "5"),
    ("six", "6"),
    ("seven", "7"),
    ("eight", "8"),
    ("nine", "9"),
    ("ten", "10"),
    ("eleven", "11"),
    ("twelve", "12"),
];

const MIN_CHARS: usize = 20;
const MAX_CHARS: usize = 420;
/// Teto do corpo da resposta do modelo.
const MAX_BODY: usize = 64 * 1024;
/// Teto do cabecalho HTTP da resposta.
const MAX_HEAD: usize = 16 * 1024;

/// Uma mensagem do chat (`role` + `content`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

fn msg(role: &str, content: &str) -> Message {
    Message {
        role: role.into(),
        content: content.into(),
    }
}

/// As quatro mensagens: sistema, o exemplo (entrada e saida) e os achados.
pub fn messages(fs: &[Finding]) -> Vec<Message> {
    vec![
        msg("system", SYSTEM),
        msg("user", EXAMPLE_IN),
        msg("assistant", EXAMPLE_OUT),
        msg("user", &facts(fs)),
    ]
}

// ------------------------------------------------------------------- guard

/// Por que a saida do modelo foi recusada (frase curta).
#[derive(Debug, Clone, PartialEq)]
pub struct Rejection {
    pub reason: String,
}

fn rejected(reason: impl Into<String>) -> Rejection {
    Rejection {
        reason: reason.into(),
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

/// Troca `<|...|>` e `<think>`/`</think>` por um espaco.
fn strip_special(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let skip = if let Some(body) = tail.strip_prefix("<|") {
            // <| [^|>]* |>
            let n = body.find(['|', '>']).unwrap_or(body.len());
            body[n..].starts_with("|>").then_some(2 + n + 2)
        } else if tail.starts_with("<think>") {
            Some("<think>".len())
        } else if tail.starts_with("</think>") {
            Some("</think>".len())
        } else {
            None
        };
        match skip {
            Some(n) => {
                out.push(' ');
                rest = &tail[n..];
            }
            None => {
                out.push('<');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Tira marcas especiais, fica com o primeiro paragrafo e acha os espacos.
fn clean(text: &str) -> String {
    let stripped = strip_special(text);
    let first = stripped.trim().split("\n\n").next().unwrap_or("");
    first.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Numeros no texto: digitos com uma parte decimal opcional (`91`, `2.6`).
fn numbers(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        out.push(&text[start..i]);
    }
    out
}

/// O texto com `one`..`twelve` (palavra inteira) trocados por digitos.
fn spell_to_digits(low: &str) -> String {
    let mut out = String::with_capacity(low.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        match WORDS.iter().find(|(w, _)| *w == word.as_str()) {
            Some((_, d)) => out.push_str(d),
            None => out.push_str(word),
        }
        word.clear();
    };
    for c in low.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Limpa e confere a saida do modelo; devolve o texto limpo ou o motivo da recusa.
/// Ordem: vazio, longo, fora do papel, numero inventado, fato-chave ausente.
pub fn guard(text: &str, fs: &[Finding]) -> Result<String, Rejection> {
    let t = clean(text);
    let len = t.chars().count();
    if len < MIN_CHARS {
        return Err(rejected("empty"));
    }
    if len > MAX_CHARS {
        return Err(rejected("too long"));
    }
    let low = t.to_lowercase();
    if let Some(p) = OOC.iter().find(|p| low.contains(**p)) {
        return Err(rejected(format!("out of role: {p}")));
    }
    let known = facts(fs);
    let mut allowed = numbers(&known);
    allowed.extend(["1", "2"]);
    for n in numbers(&t) {
        // zeros finais so contam depois de um ponto decimal (a referencia
        // Python aceitava 20 e 100 porque "2" e "1" sempre estao permitidos)
        let trimmed = if n.contains('.') {
            n.trim_end_matches('0').trim_end_matches('.')
        } else {
            n
        };
        if !allowed.contains(&n) && !allowed.contains(&trimmed) {
            return Err(rejected(format!("number not in findings: {n}")));
        }
    }
    if let Some(first) = fs.first() {
        let spelled = spell_to_digits(&low);
        for need in &first.must {
            let need = need.to_lowercase();
            if !low.contains(&need) && !spelled.contains(&need) {
                return Err(rejected(format!("missing key fact: {need}")));
            }
        }
    }
    Ok(t)
}

// ---------------------------------------------------------------- endpoint

/// Endereco do servidor do modelo: so loopback, resolvido sem DNS.
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    addr: SocketAddr,
}

/// Endpoint recusado.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointError(pub String);

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "endpoint refused: {}", self.0)
    }
}

impl std::error::Error for EndpointError {}

fn refuse<T>(why: &str) -> Result<T, EndpointError> {
    Err(EndpointError(why.into()))
}

impl Endpoint {
    /// Aceita so `http://127.0.0.1:<porta>`, `http://localhost:<porta>` e `http://[::1]:<porta>`,
    /// com `/` ou `/v1` (com ou sem barra final) opcionais. O resto e erro: outro host, https,
    /// usuario, query, fragmento, outro caminho. `localhost` vira 127.0.0.1 sem consultar DNS.
    pub fn parse(s: &str) -> Result<Endpoint, EndpointError> {
        let Some(rest) = s.strip_prefix("http://") else {
            return refuse("only http:// is allowed");
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        if !matches!(tail, "" | "/" | "/v1" | "/v1/") {
            return refuse("path must be empty or /v1");
        }
        let (ip, port) = if let Some(p) = authority.strip_prefix("[::1]:") {
            (IpAddr::V6(Ipv6Addr::LOCALHOST), p)
        } else if let Some((host, p)) = authority.split_once(':') {
            match host {
                "127.0.0.1" | "localhost" => (IpAddr::V4(Ipv4Addr::LOCALHOST), p),
                _ => return refuse("host must be 127.0.0.1, localhost or [::1]"),
            }
        } else {
            return refuse("host must be 127.0.0.1, localhost or [::1], with a port");
        };
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return refuse("port must be digits");
        }
        match port.parse::<u16>() {
            Ok(p) if p != 0 => Ok(Endpoint {
                addr: SocketAddr::new(ip, p),
            }),
            _ => refuse("port out of range"),
        }
    }

    /// O endereco de loopback ja resolvido.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

// ------------------------------------------------------------------ client

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
/// `timeout` e o prazo total (conexao, escrita e leitura juntas).
pub fn ask(
    endpoint: &Endpoint,
    messages: &[Message],
    timeout: Duration,
) -> Result<String, ClientError> {
    let deadline = Instant::now() + timeout;
    let body = json!({
        "model": "local",
        "messages": messages,
        "max_tokens": 120,
        "temperature": 0,
    })
    .to_string();
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
         Accept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        endpoint.addr,
        body.len()
    );
    let mut s = TcpStream::connect_timeout(&endpoint.addr, remaining(deadline)?).map_err(io_err)?;
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

// ------------------------------------------------------------------- brief

/// De onde veio o texto mostrado.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Rules,
    Model,
}

/// O briefing mostrado; `rejected` diz por que o modelo nao foi usado (se foi tentado).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Briefing {
    pub text: String,
    pub source: Source,
    pub rejected: Option<String>,
}

/// O texto do modelo se houver endpoint e a resposta passar o guard; senao o texto de regras.
/// Sem endpoint nao ha o que recusar (`rejected` fica vazio); erro do cliente, timeout e recusa
/// do guard trazem o motivo.
pub fn brief(fs: &[Finding], endpoint: Option<&Endpoint>, timeout: Duration) -> Briefing {
    let rules = |rejected: Option<String>| Briefing {
        text: fallback(fs),
        source: Source::Rules,
        rejected,
    };
    let Some(endpoint) = endpoint else {
        return rules(None);
    };
    match ask(endpoint, &messages(fs), timeout) {
        Err(e) => rules(Some(e.to_string())),
        Ok(raw) => match guard(&raw, fs) {
            Ok(text) => Briefing {
                text,
                source: Source::Model,
                rejected: None,
            },
            Err(r) => rules(Some(r.reason)),
        },
    }
}

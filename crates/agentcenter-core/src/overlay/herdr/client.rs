//! Cliente do socket API do herdr (protocolo 22): um objeto JSON por linha.
//!
//! O servidor responde uma requisicao por conexao e fecha; so
//! `events.subscribe` mantem a conexao aberta e passa a mandar um evento por
//! linha. Somente leitura: o cliente nunca manda comando que mude o herdr.

use serde_json::Value;
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::net::{SocketAddr, UnixStream};
use std::time::Duration;

/// Maior linha aceita do servidor. `agent.list` com 30 agentes tem ~15 KiB;
/// acima disso a resposta e tratada como erro em vez de crescer sem limite.
pub const MAX_LINE: usize = 4 << 20;

/// `timeout` so cobre leitura e escrita. O `connect` em si nao tem prazo: a std
/// nao da `connect_timeout` para socket Unix e o crate nao depende de `libc`.
/// Ele so bloqueia com a fila de `accept` do servidor cheia, e bloqueia a
/// thread do overlay, nunca o tick.
fn connect(addr: &SocketAddr, timeout: Duration) -> io::Result<UnixStream> {
    let s = UnixStream::connect_addr(addr)?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    Ok(s)
}

fn send(s: &mut UnixStream, id: &str, method: &str, params: Value) -> io::Result<()> {
    let mut line = serde_json::to_vec(&serde_json::json!({
        "id": id,
        "method": method,
        "params": params,
    }))?;
    line.push(b'\n');
    s.write_all(&line)
}

/// Le uma linha inteira para `buf` (sem o `\n`). `Ok(false)`: o tempo acabou
/// antes do fim da linha; o que ja chegou fica em `buf` para a proxima chamada.
fn read_line(r: &mut impl BufRead, buf: &mut Vec<u8>) -> io::Result<bool> {
    loop {
        if buf.len() > MAX_LINE {
            return Err(ErrorKind::InvalidData.into());
        }
        // Pelo menos 1: `Ok(0)` abaixo so pode ser fim da conexao.
        let room = (MAX_LINE + 1 - buf.len()) as u64;
        match r.by_ref().take(room).read_until(b'\n', buf) {
            Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
            Ok(_) if buf.last() == Some(&b'\n') => {
                buf.pop();
                return Ok(true);
            }
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Ok(false);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// Uma requisicao: conecta, manda, le a linha de resposta para `buf`.
/// `timeout` vale para cada leitura e escrita.
pub fn request(
    addr: &SocketAddr,
    id: &str,
    method: &str,
    params: Value,
    timeout: Duration,
    buf: &mut Vec<u8>,
) -> io::Result<()> {
    let mut s = connect(addr, timeout)?;
    send(&mut s, id, method, params)?;
    buf.clear();
    let mut r = BufReader::new(s);
    if read_line(&mut r, buf)? {
        Ok(())
    } else {
        Err(ErrorKind::TimedOut.into())
    }
}

/// Evento recebido de uma assinatura.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `pane.agent_status_changed`: so o status do pane mudou.
    Status { pane_id: String, status: String },
    /// Qualquer outro evento: a topologia ou os agentes podem ter mudado.
    Changed,
    /// Linha que nao e evento (resposta, erro): ignorada.
    Other,
}

/// Interpreta uma linha da assinatura. Linha invalida conta como [`Event::Other`].
pub fn parse_event(line: &[u8]) -> Event {
    let Ok(v) = serde_json::from_slice::<Value>(line) else {
        return Event::Other;
    };
    let Some(kind) = v.get("event").and_then(Value::as_str) else {
        return Event::Other;
    };
    if kind == "pane.agent_status_changed" {
        let data = v.get("data");
        let text = |k: &str| {
            data.and_then(|d| d.get(k))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        if let (Some(pane_id), Some(status)) = (text("pane_id"), text("agent_status")) {
            return Event::Status { pane_id, status };
        }
    }
    Event::Changed
}

/// Conexao de `events.subscribe` aberta.
pub struct Subscription {
    reader: BufReader<UnixStream>,
    buf: Vec<u8>,
}

impl Subscription {
    /// Assina `subscriptions` (objetos `{"type": ...}`). So devolve a
    /// assinatura se o servidor confirmou com `subscription_started`.
    /// `wait` e quanto cada [`Subscription::next_event`] espera por um evento.
    pub fn open(
        addr: &SocketAddr,
        id: &str,
        subscriptions: Vec<Value>,
        timeout: Duration,
        wait: Duration,
    ) -> io::Result<Self> {
        let mut s = connect(addr, timeout)?;
        send(
            &mut s,
            id,
            "events.subscribe",
            serde_json::json!({ "subscriptions": subscriptions }),
        )?;
        let mut sub = Self {
            reader: BufReader::new(s),
            buf: Vec::new(),
        };
        if !read_line(&mut sub.reader, &mut sub.buf)? {
            return Err(ErrorKind::TimedOut.into());
        }
        let started = serde_json::from_slice::<Value>(&sub.buf)
            .ok()
            .and_then(|v| {
                v.get("result")?
                    .get("type")?
                    .as_str()
                    .map(|t| t == "subscription_started")
            })
            .unwrap_or(false);
        if !started {
            return Err(ErrorKind::InvalidData.into());
        }
        sub.buf.clear();
        sub.reader.get_ref().set_read_timeout(Some(wait))?;
        Ok(sub)
    }

    /// Proximo evento; `Ok(None)` se nada chegou no tempo de espera.
    /// `Err`: conexao fechada ou quebrada (servidor parou, reiniciou).
    pub fn next_event(&mut self) -> io::Result<Option<Event>> {
        if !read_line(&mut self.reader, &mut self.buf)? {
            return Ok(None);
        }
        let ev = parse_event(&self.buf);
        self.buf.clear();
        Ok(Some(ev))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn herdr_event_status_change_is_parsed() {
        let line = br#"{"data":{"agent":"claude","agent_status":"working","pane_id":"w1:p2","workspace_id":"w1","extra":1},"event":"pane.agent_status_changed"}"#;
        assert_eq!(
            parse_event(line),
            Event::Status {
                pane_id: "w1:p2".into(),
                status: "working".into()
            }
        );
    }

    #[test]
    fn herdr_event_other_kinds_and_garbage() {
        let renamed = br#"{"data":{"tab_id":"w1:t1","type":"tab_renamed"},"event":"tab_renamed"}"#;
        assert_eq!(parse_event(renamed), Event::Changed);
        // Status sem pane_id: nao da para aplicar, entao vale como mudanca geral.
        let partial = br#"{"data":{"agent_status":"idle"},"event":"pane.agent_status_changed"}"#;
        assert_eq!(parse_event(partial), Event::Changed);
        assert_eq!(
            parse_event(br#"{"id":"s","result":{"type":"subscription_started"}}"#),
            Event::Other
        );
        assert_eq!(parse_event(b"not json"), Event::Other);
        assert_eq!(parse_event(b"[1,2]"), Event::Other);
    }

    #[test]
    fn herdr_read_line_keeps_partial_line_across_timeouts() {
        // Cursor nunca da timeout; simula com dois pedacos lidos em sequencia.
        let mut buf = b"{\"a\":".to_vec();
        let mut r = io::Cursor::new(b"1}\n{\"b\":2}\n".to_vec());
        assert!(read_line(&mut r, &mut buf).unwrap());
        assert_eq!(buf, b"{\"a\":1}");
        buf.clear();
        assert!(read_line(&mut r, &mut buf).unwrap());
        assert_eq!(buf, b"{\"b\":2}");
        buf.clear();
        assert_eq!(
            read_line(&mut r, &mut buf).unwrap_err().kind(),
            ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn herdr_read_line_rejects_oversized_line() {
        let mut big = vec![b'x'; MAX_LINE + 10];
        big.push(b'\n');
        let mut r = io::Cursor::new(big);
        let mut buf = Vec::new();
        assert_eq!(
            read_line(&mut r, &mut buf).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        assert!(buf.len() <= MAX_LINE + 1);
    }
}

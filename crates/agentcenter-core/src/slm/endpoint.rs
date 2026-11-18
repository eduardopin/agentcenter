//! Endereco do servidor do modelo: so loopback, resolvido sem DNS. Um endereco recusado e erro de
//! uso (exit 2 na CLI) e nunca chega a abrir conexao.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

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

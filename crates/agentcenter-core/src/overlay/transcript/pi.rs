//! pi: `~/.pi/agent/sessions/--<cwd>--/<inicio>_<id>.jsonl`.
//!
//! Conta de `scan_pi` em `contexto.py`: o modelo vem de `model_change`
//! (`modelId`); cada `message` de `role` `assistant` com `usage` e um pedido
//! (`input`, `cacheRead`, `cacheWrite`, `output` somados), e o contexto do
//! pedido e `input` + `cacheRead` + `cacheWrite`. O pi nao diz a janela.
//!
//! Tool calls: bloco `toolCall` (`id`) no `content` de uma mensagem
//! `assistant`; resultado, mensagem `toolResult` (`toolCallId`). `name` e
//! `arguments` (ou `input`) viram so o hash do sinal de loop
//! ([`tools::call_key`], C23), que, como o script, olha os blocos `toolCall`,
//! `tool_use` e `tool_call` (o total so conta `toolCall`). `content` nao e
//! lido. Estado pela ultima `assistant`:
//! `stopReason` `toolUse` e `busy` (o pi roda a ferramenta e volta ao
//! modelo); outro (`stop`, `error`, ...) e `idle`; sem `stopReason`, `busy` se
//! ela pediu ferramenta.
//!
//! Provider: o `provider` de `model_change` e o da mensagem `assistant`, o
//! ultimo visto vence. Id da sessao: o `id` da linha `session`.
//!
//! Casamento: o pi abre e fecha o arquivo a cada escrita (nenhum fd aberto
//! nesta maquina), entao vale o cabecalho (`session`, com `cwd` e inicio) e o
//! diretorio do `cwd`.

use super::tools::{self, Args, Empty};
use super::{Header, Usage, lenient, parse_iso_ms, parse_line};
use serde::Deserialize;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type", default, deserialize_with = "lenient::string")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    timestamp: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    cwd: Option<String>,
    #[serde(rename = "modelId", default, deserialize_with = "lenient::string")]
    model_id: Option<String>,
    /// `model_change`: provider do modelo novo.
    #[serde(default, deserialize_with = "lenient::string")]
    provider: Option<String>,
    /// Id da linha; so o da linha `session` e usado (o id da sessao).
    #[serde(default, deserialize_with = "lenient::string")]
    id: Option<String>,
    #[serde(default, deserialize_with = "lenient::object")]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default, deserialize_with = "lenient::string")]
    role: Option<String>,
    #[serde(default, deserialize_with = "lenient::object")]
    usage: Option<Tokens>,
    #[serde(default, deserialize_with = "lenient::list")]
    content: Vec<Block>,
    #[serde(rename = "toolCallId", default, deserialize_with = "lenient::hash")]
    tool_call_id: Option<u64>,
    #[serde(rename = "stopReason", default, deserialize_with = "lenient::string")]
    stop_reason: Option<String>,
    /// `assistant`: provider que respondeu.
    #[serde(default, deserialize_with = "lenient::string")]
    provider: Option<String>,
}

/// Bloco de `content`: o tipo, o id e nome e argumentos (em hash).
#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type", default, deserialize_with = "lenient::string")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient::hash")]
    id: Option<u64>,
    #[serde(default, deserialize_with = "tools::name")]
    name: Option<u64>,
    #[serde(default, deserialize_with = "tools::args")]
    arguments: Args,
    #[serde(default, deserialize_with = "tools::args")]
    input: Args,
}

#[derive(Deserialize)]
struct Tokens {
    #[serde(default, deserialize_with = "lenient::count")]
    input: u64,
    #[serde(rename = "cacheRead", default, deserialize_with = "lenient::count")]
    cache_read: u64,
    #[serde(rename = "cacheWrite", default, deserialize_with = "lenient::count")]
    cache_write: u64,
    #[serde(default, deserialize_with = "lenient::count")]
    output: u64,
}

/// Aplica uma linha do transcript.
pub fn feed(line: &[u8], u: &mut Usage) {
    let Some(d) = parse_line::<Line>(line) else {
        return;
    };
    let at = u.set_timestamp(d.timestamp.as_deref());
    match d.kind.as_deref() {
        Some("session") => u.set_session_id(d.id),
        Some("model_change") => {
            u.set_model(d.model_id);
            u.set_provider(d.provider);
        }
        Some("message") => {
            let Some(m) = d.message else {
                return;
            };
            match m.role.as_deref() {
                Some("assistant") => {}
                Some("toolResult") => {
                    if let Some(id) = m.tool_call_id {
                        u.tools.answered(id);
                    }
                    return;
                }
                _ => return,
            }
            u.set_provider(m.provider);
            let mut asked = false;
            for b in &m.content {
                let kind = b.kind.as_deref();
                if kind == Some("toolCall") {
                    u.tools.called(b.id, at);
                    asked = true;
                }
                if matches!(kind, Some("toolCall" | "tool_use" | "tool_call"))
                    && let Some(k) = tools::call_key(b.name, b.arguments.or(b.input), Empty::Object)
                {
                    u.tools.repeated(k);
                }
            }
            u.busy = Some(match m.stop_reason.as_deref() {
                Some(r) => r == "toolUse",
                None => asked,
            });
            if let Some(t) = m.usage {
                u.requests = u.requests.saturating_add(1);
                u.input = u.input.saturating_add(t.input);
                u.cache_read = u.cache_read.saturating_add(t.cache_read);
                u.cache_write = u.cache_write.saturating_add(t.cache_write);
                u.output = u.output.saturating_add(t.output);
                u.set_context(
                    t.input
                        .saturating_add(t.cache_read)
                        .saturating_add(t.cache_write),
                );
            }
        }
        _ => {}
    }
}

/// `cwd` e inicio, da linha `session`.
pub fn header(line: &[u8]) -> Header {
    match parse_line::<Line>(line) {
        Some(d) if d.kind.as_deref() == Some("session") => Header {
            cwd: d.cwd,
            started_ms: d.timestamp.as_deref().and_then(parse_iso_ms),
        },
        _ => Header::default(),
    }
}

/// Diretorio de sessoes do `cwd` como o pi nomeia: sem a `/` inicial, `/`,
/// `\` e `:` viram `-`, entre `--` (`/home/a/b_c` -> `--home-a-b_c--`).
pub fn slug(cwd: &str) -> String {
    let rest = cwd.strip_prefix(['/', '\\']).unwrap_or(cwd);
    let body: String = rest
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("--{body}--")
}

/// `.jsonl` do diretorio do `cwd`, com o mtime em ms.
pub fn candidates(root: &Path, cwd: &str) -> Vec<(PathBuf, u64)> {
    let Ok(rd) = std::fs::read_dir(root.join(slug(cwd))) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(".jsonl"))
        })
        .filter_map(|e| {
            let m = e.metadata().ok().filter(|m| m.is_file())?;
            let mtime = m.mtime().max(0) as u64 * 1000 + m.mtime_nsec() as u64 / 1_000_000;
            Some((e.path(), mtime))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_pi_session_dirs() {
        assert_eq!(slug("/home/dev"), "--home-dev--");
        assert_eq!(
            slug("/home/dev/work/demo_project"),
            "--home-dev-work-demo_project--"
        );
    }

    #[test]
    fn assistant_usage_accumulates_and_model_comes_from_model_change() {
        let mut u = Usage::default();
        feed(br#"{"type":"model_change","modelId":"m1"}"#, &mut u);
        let msg = br#"{"type":"message","message":{"role":"assistant","model":"ignorado","usage":{"input":1,"output":2,"cacheRead":3,"cacheWrite":4}}}"#;
        feed(msg, &mut u);
        feed(msg, &mut u);
        feed(
            br#"{"type":"message","message":{"role":"user","usage":{"input":99}}}"#,
            &mut u,
        );
        assert_eq!(
            (u.requests, u.tokens_in(), u.output, u.context),
            (2, 16, 4, 8)
        );
        assert_eq!(u.model.as_deref(), Some("m1"));
    }

    #[test]
    fn tool_calls_by_id_and_stop_reason_gives_the_state() {
        let mut u = Usage::default();
        let asst = |blocks: &str, stop: &str| {
            format!(
                "{{\"type\":\"message\",\"timestamp\":\"2026-10-17T02:53:35.000Z\",\
                 \"message\":{{\"role\":\"assistant\",\"content\":[{blocks}]{stop}}}}}"
            )
        };
        let call = |id: &str| {
            format!(
                "{{\"type\":\"toolCall\",\"id\":\"{id}\",\"name\":\"read\",\"arguments\":{{}}}}"
            )
        };
        let result = |id: &str| {
            format!(
                "{{\"type\":\"message\",\"message\":{{\"role\":\"toolResult\",\"toolCallId\":\"{id}\"}}}}"
            )
        };
        feed(
            asst(
                &format!("{},{}", call("t1"), call("t2")),
                ",\"stopReason\":\"toolUse\"",
            )
            .as_bytes(),
            &mut u,
        );
        assert_eq!((u.tools.total, u.tools.open(), u.busy), (2, 2, Some(true)));
        feed(result("t1").as_bytes(), &mut u);
        feed(result("t2").as_bytes(), &mut u);
        assert_eq!((u.tools.open(), u.busy), (0, Some(true)), "volta ao modelo");
        feed(asst("", ",\"stopReason\":\"stop\"").as_bytes(), &mut u);
        assert_eq!(u.busy, Some(false));
        feed(asst(&call("t3"), "").as_bytes(), &mut u);
        assert_eq!((u.tools.open(), u.busy), (1, Some(true)), "sem stopReason");
        feed(asst("", ",\"stopReason\":\"error\"").as_bytes(), &mut u);
        assert_eq!(u.busy, Some(false));
        // toolCall fora de `assistant` nao conta.
        feed(br#"{"type":"message","message":{"role":"user","content":[{"type":"toolCall","id":"t9"}]}}"#, &mut u);
        assert_eq!(u.tools.total, 3);
    }
}

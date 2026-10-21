//! Claude Code: `~/.claude/projects/<slug>/<sessionId>.jsonl`.
//!
//! Conta de `scan_claude` em `contexto.py`: as linhas `assistant` trazem
//! `message.model` e `message.usage`; o mesmo pedido aparece em varias linhas
//! (uma por bloco de conteudo) com o mesmo `requestId`, e so a primeira de uma
//! sequencia conta. Contexto do pedido = `input_tokens` +
//! `cache_creation_input_tokens` + `cache_read_input_tokens`.
//!
//! Tool calls: bloco `tool_use` (`id`) no `message.content` de uma linha
//! `assistant`; resultado, bloco `tool_result` (`tool_use_id`) numa linha
//! `user`. Linhas de outro tipo (`api-request-blob` repete os dois) nao contam.
//! Do bloco sao lidos `type` e o id; do `tool_use`, tambem `name` e `input`,
//! que viram so o hash do sinal de loop ([`tools::call_key`], C23, com o
//! `input or {}` do script). `content` nao.

use super::tools::{self, Args, Empty};
use super::{Usage, lenient, parse_line};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type", default, deserialize_with = "lenient::string")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    timestamp: Option<String>,
    #[serde(rename = "requestId", default, deserialize_with = "lenient::string")]
    request_id: Option<String>,
    #[serde(default, deserialize_with = "lenient::object")]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default, deserialize_with = "lenient::string")]
    model: Option<String>,
    #[serde(default, deserialize_with = "lenient::object")]
    usage: Option<Tokens>,
    #[serde(default, deserialize_with = "lenient::list")]
    content: Vec<Block>,
}

/// Bloco de `message.content`: o tipo, os ids e nome e argumentos (em hash).
#[derive(Deserialize)]
struct Block {
    #[serde(default, deserialize_with = "tools::name")]
    name: Option<u64>,
    #[serde(default, deserialize_with = "tools::args")]
    input: Args,
    #[serde(rename = "type", default, deserialize_with = "lenient::string")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient::hash")]
    id: Option<u64>,
    #[serde(default, deserialize_with = "lenient::hash")]
    tool_use_id: Option<u64>,
}

#[derive(Deserialize)]
struct Tokens {
    #[serde(default, deserialize_with = "lenient::count")]
    input_tokens: u64,
    #[serde(default, deserialize_with = "lenient::count")]
    cache_creation_input_tokens: u64,
    #[serde(default, deserialize_with = "lenient::count")]
    cache_read_input_tokens: u64,
    #[serde(default, deserialize_with = "lenient::count")]
    output_tokens: u64,
}

/// Aplica uma linha do transcript.
pub fn feed(line: &[u8], u: &mut Usage) {
    let Some(d) = parse_line::<Line>(line) else {
        return;
    };
    let at = u.set_timestamp(d.timestamp.as_deref());
    let Some(m) = d.message else {
        return;
    };
    match d.kind.as_deref() {
        Some("assistant") => {}
        Some("user") => {
            for b in &m.content {
                if b.kind.as_deref() == Some("tool_result")
                    && let Some(id) = b.tool_use_id
                {
                    u.tools.answered(id);
                }
            }
            return;
        }
        _ => return,
    }
    for b in &m.content {
        if b.kind.as_deref() == Some("tool_use") {
            u.tools.called(b.id, at);
            if let Some(k) = tools::call_key(b.name, b.input, Empty::Object) {
                u.tools.repeated(k);
            }
        }
    }
    u.set_model(m.model);
    // `if u and rid != seen`: sem requestId nas duas pontas nao conta.
    if let Some(t) = m.usage
        && d.request_id != u.last_request
    {
        u.last_request = d.request_id;
        u.requests = u.requests.saturating_add(1);
        u.input = u.input.saturating_add(t.input_tokens);
        u.cache_write = u.cache_write.saturating_add(t.cache_creation_input_tokens);
        u.cache_read = u.cache_read.saturating_add(t.cache_read_input_tokens);
        u.output = u.output.saturating_add(t.output_tokens);
        u.set_context(
            t.input_tokens
                .saturating_add(t.cache_creation_input_tokens)
                .saturating_add(t.cache_read_input_tokens),
        );
    }
}

/// Diretorio do projeto como o Claude nomeia: todo caractere que nao e letra
/// ou digito ASCII vira `-` (`/home/a/b_c` -> `-home-a-b-c`).
pub fn slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Transcript da sessao: primeiro pelo slug do `cwd`; senao o
/// `<sessionId>.jsonl` de `projects/*/` (o mais recente, se houver mais de um).
pub fn find(root: &Path, cwd: Option<&str>, session_id: &str) -> Option<PathBuf> {
    let name = format!("{session_id}.jsonl");
    if let Some(cwd) = cwd {
        let p = root.join(slug(cwd)).join(&name);
        if p.is_file() {
            return Some(p);
        }
    }
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let p = e.path().join(&name);
            let m = std::fs::metadata(&p).ok().filter(|m| m.is_file())?;
            Some((m.modified().ok(), p))
        })
        .max()
        .map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cockpit::Loops;

    #[test]
    fn slug_matches_claude_project_dirs() {
        assert_eq!(
            slug("/home/dev/work/demo_project"),
            "-home-dev-work-demo-project"
        );
        assert_eq!(slug("/a/.claude/w"), "-a--claude-w");
    }

    #[test]
    fn repeated_request_id_counts_once_and_missing_ids_never_count() {
        let mut u = Usage::default();
        let line = |rid: &str| {
            format!(
                "{{\"type\":\"assistant\",{rid}\"message\":{{\"model\":\"m\",\"usage\":\
                 {{\"input_tokens\":1,\"cache_creation_input_tokens\":2,\
                 \"cache_read_input_tokens\":3,\"output_tokens\":4}}}}}}"
            )
        };
        feed(line("").as_bytes(), &mut u);
        assert_eq!(u.requests, 0, "sem requestId: None == None, nao conta");
        feed(line("\"requestId\":\"r1\",").as_bytes(), &mut u);
        feed(line("\"requestId\":\"r1\",").as_bytes(), &mut u);
        assert_eq!(u.requests, 1);
        feed(line("\"requestId\":\"r2\",").as_bytes(), &mut u);
        assert_eq!(
            (u.requests, u.tokens_in(), u.output, u.context),
            (2, 12, 8, 6)
        );
        assert_eq!(u.model.as_deref(), Some("m"));
    }

    #[test]
    fn tool_use_in_assistant_and_tool_result_in_user_lines_only() {
        let mut u = Usage::default();
        let line = |kind: &str, block: &str| {
            format!(
                "{{\"type\":\"{kind}\",\"timestamp\":\"2026-10-15T10:00:00.000Z\",\
                 \"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"x\"}},{block}]}}}}"
            )
        };
        let call = |id: &str| format!("{{\"type\":\"tool_use\",\"id\":\"{id}\",\"input\":{{}}}}");
        let done = |id: &str| format!("{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\"}}");
        feed(line("assistant", &call("a")).as_bytes(), &mut u);
        feed(line("assistant", &call("b")).as_bytes(), &mut u);
        feed(line("api-request-blob", &call("c")).as_bytes(), &mut u);
        feed(line("api-request-blob", &done("a")).as_bytes(), &mut u);
        assert_eq!((u.tools.total, u.tools.open()), (2, 2));
        feed(line("user", &done("a")).as_bytes(), &mut u);
        feed(line("assistant", &done("b")).as_bytes(), &mut u); // so em `user`
        assert_eq!((u.tools.total, u.tools.open()), (2, 1));
        // `content` texto (prompt) ou de tipo estranho nao derruba a linha.
        feed(br#"{"type":"user","message":{"content":"oi"}}"#, &mut u);
        feed(
            br#"{"type":"user","message":{"content":[1,"x",{"type":7}]}}"#,
            &mut u,
        );
        assert_eq!(u.busy, None, "Claude: estado e do claude_state");
    }

    /// O Python le surrogate solto e numero alem do `f64`; o serde nao. A
    /// linha conta (requisicao e chamada), a chamada fica fora da janela do
    /// loop, e a linha seguinte volta a ler os argumentos.
    #[test]
    fn unreadable_tool_arguments_keep_the_line_and_skip_the_loop_key() {
        let mut u = Usage::default();
        let line = |rid: &str, input: &str| {
            format!(
                "{{\"type\":\"assistant\",\"requestId\":\"{rid}\",\"message\":{{\"model\":\"m\",\
                 \"content\":[{{\"type\":\"tool_use\",\"id\":\"{rid}\",\"name\":\"Bash\",\"input\":{input}}}],\
                 \"usage\":{{\"input_tokens\":1,\"output_tokens\":2}}}}}}"
            )
        };
        let surrogate = r#"{"command":"echo \ud800"}"#;
        let huge = r#"{"command":"echo","n":1e400}"#;
        for (rid, input) in [("r1", surrogate), ("r2", huge), ("r3", surrogate)] {
            feed(line(rid, input).as_bytes(), &mut u);
        }
        assert_eq!(
            (u.requests, u.input, u.output),
            (3, 3, 6),
            "as linhas contam"
        );
        assert_eq!((u.tools.total, u.tools.open()), (3, 3));
        let none = Loops {
            max_streak: 1,
            repeated_max: 0,
        };
        assert_eq!(u.tools.loops(), none, "fora da janela: nao viram 3 iguais");
        // O flag de pular nao fica preso: a seguinte e lida inteira.
        for rid in ["r4", "r5"] {
            feed(line(rid, r#"{"command":"echo ok"}"#).as_bytes(), &mut u);
        }
        assert_eq!(u.tools.loops().repeated_max, 2);
        assert_eq!(u.requests, 5);
    }

    #[test]
    fn hostile_token_counts_saturate_instead_of_overflowing() {
        let mut u = Usage::default();
        let max = u64::MAX;
        for rid in ["r1", "r2"] {
            let line = format!(
                "{{\"type\":\"assistant\",\"requestId\":\"{rid}\",\"message\":{{\"model\":\"m\",\
                 \"usage\":{{\"input_tokens\":{max},\"cache_creation_input_tokens\":{max},\
                 \"cache_read_input_tokens\":{max},\"output_tokens\":{max}}}}}}}"
            );
            feed(line.as_bytes(), &mut u);
        }
        assert_eq!(u.requests, 2);
        assert_eq!((u.tokens_in(), u.output, u.context), (max, max, max));
    }

    #[test]
    fn odd_field_types_do_not_drop_the_line() {
        let mut u = Usage::default();
        feed(
            br#"{"type":"assistant","requestId":"r","message":{"model":7,"content":"x","usage":{"input_tokens":"9","output_tokens":5}}}"#,
            &mut u,
        );
        assert_eq!((u.requests, u.input, u.output), (1, 0, 5));
        assert_eq!(u.model, None);
        feed(b"not json", &mut u);
        feed(br#"{"type":"assistant","message":"texto"}"#, &mut u);
        assert_eq!(u.requests, 1);
    }
}

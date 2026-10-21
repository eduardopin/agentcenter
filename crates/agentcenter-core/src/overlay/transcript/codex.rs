//! Codex: `~/.codex/sessions/AAAA/MM/DD/rollout-<inicio>-<id>.jsonl`.
//!
//! Conta de `scan_codex` em `contexto.py`: o modelo vem de `turn_context`; cada
//! `event_msg` do tipo `token_count` traz os totais da sessao
//! (`info.total_token_usage`, que substituem os anteriores, com `info` nulo
//! zerando tudo) e o ultimo pedido (`info.last_token_usage`). Entrada sem
//! cache = `input_tokens` - `cached_input_tokens`; contexto do pedido =
//! `input_tokens` + `cache_write_input_tokens` do ultimo; janela =
//! `info.model_context_window`.
//!
//! Tool calls: `response_item` com `payload.type` `function_call` ou
//! `custom_tool_call` (`call_id`); resultado, o `*_output` do mesmo
//! `call_id`. `name` e `arguments` (ou `input`) viram so o hash do sinal de
//! loop ([`tools::call_key`], C23, com o `arguments or input or ""` do
//! script); `output` nao e lido. Estado:
//! `event_msg` `task_started` abre o turno (`busy`), `task_complete` ou
//! `turn_aborted` fecha (`idle`); sem nenhum dos tres, sem estado.
//!
//! Provider e id da sessao: `model_provider` e `id` do `payload` da linha
//! `session_meta` (o resto dela, inclusive `base_instructions`, nao e lido).
//!
//! Casamento: o Codex mantem o rollout aberto enquanto grava, entao o fd em
//! `/proc/<pid>/fd` resolve. O cabecalho (`session_meta`) da `cwd` e inicio
//! para o caso sem fd.
//!
//! Subagentes (C15, ver [`super::subagents`]): cada um e um rollout proprio
//! cujo `session_meta.payload.source.subagent.thread_spawn.parent_thread_id`
//! e o id do rollout pai ([`spawn_parent`]); so esse campo e lido.

use super::tools::{self, Args, Empty};
use super::{
    Header, Usage, civil_from_days, days_from_civil, lenient, parse_iso_ms, parse_line,
    valid_session_id,
};
use serde::Deserialize;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Dias de diretorio olhados no casamento sem fd (o processo mais antigo que
/// isso so casa pelo fd).
const MAX_DAYS: i64 = 10;

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type", default, deserialize_with = "lenient::string")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    timestamp: Option<String>,
    #[serde(default, deserialize_with = "lenient::object")]
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type", default, deserialize_with = "lenient::string")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    model: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    cwd: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    timestamp: Option<String>,
    #[serde(default, deserialize_with = "lenient::object")]
    info: Option<Info>,
    #[serde(default, deserialize_with = "lenient::hash")]
    call_id: Option<u64>,
    /// Tool call: nome e argumentos, so em hash.
    #[serde(default, deserialize_with = "tools::name")]
    name: Option<u64>,
    #[serde(default, deserialize_with = "tools::args")]
    arguments: Args,
    #[serde(default, deserialize_with = "tools::args")]
    input: Args,
    /// `session_meta`: provider declarado e id da sessao.
    #[serde(default, deserialize_with = "lenient::string")]
    model_provider: Option<String>,
    #[serde(default, deserialize_with = "lenient::string")]
    id: Option<String>,
    /// `session_meta` de subagente: `source.subagent.thread_spawn`. No pai e
    /// texto (`"cli"`, `"vscode"`) e vira `None`.
    #[serde(default, deserialize_with = "lenient::object")]
    source: Option<Source>,
}

#[derive(Deserialize)]
struct Source {
    #[serde(default, deserialize_with = "lenient::object")]
    subagent: Option<Subagent>,
}

#[derive(Deserialize)]
struct Subagent {
    #[serde(default, deserialize_with = "lenient::object")]
    thread_spawn: Option<ThreadSpawn>,
}

#[derive(Deserialize)]
struct ThreadSpawn {
    #[serde(default, deserialize_with = "lenient::string")]
    parent_thread_id: Option<String>,
}

#[derive(Deserialize, Default)]
struct Info {
    #[serde(default, deserialize_with = "lenient::object")]
    total_token_usage: Option<Tokens>,
    #[serde(default, deserialize_with = "lenient::object")]
    last_token_usage: Option<Tokens>,
    #[serde(default, deserialize_with = "lenient::number")]
    model_context_window: Option<u64>,
}

#[derive(Deserialize, Default)]
struct Tokens {
    #[serde(default, deserialize_with = "lenient::count")]
    input_tokens: u64,
    #[serde(default, deserialize_with = "lenient::count")]
    cached_input_tokens: u64,
    #[serde(default, deserialize_with = "lenient::count")]
    cache_write_input_tokens: u64,
    #[serde(default, deserialize_with = "lenient::count")]
    output_tokens: u64,
}

/// Aplica uma linha do transcript.
pub fn feed(line: &[u8], u: &mut Usage) {
    let Some(d) = parse_line::<Line>(line) else {
        return;
    };
    let at = u.set_timestamp(d.timestamp.as_deref());
    let Some(p) = d.payload else {
        return;
    };
    match d.kind.as_deref() {
        Some("session_meta") => {
            u.set_provider(p.model_provider);
            u.set_session_id(p.id);
        }
        Some("turn_context") => u.set_model(p.model),
        Some("response_item") => match p.kind.as_deref() {
            Some("function_call" | "custom_tool_call") => {
                u.tools.called(p.call_id, at);
                let args = p.arguments.or(p.input);
                if let Some(k) = tools::call_key(p.name, args, Empty::Text) {
                    u.tools.repeated(k);
                }
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                if let Some(id) = p.call_id {
                    u.tools.answered(id);
                }
            }
            _ => {}
        },
        Some("event_msg") if p.kind.as_deref() == Some("task_started") => u.busy = Some(true),
        Some("event_msg")
            if matches!(p.kind.as_deref(), Some("task_complete" | "turn_aborted")) =>
        {
            u.busy = Some(false)
        }
        Some("event_msg") if p.kind.as_deref() == Some("token_count") => {
            let info = p.info.unwrap_or_default();
            let tot = info.total_token_usage.unwrap_or_default();
            let last = info.last_token_usage.unwrap_or_default();
            u.input = tot.input_tokens.saturating_sub(tot.cached_input_tokens);
            u.cache_read = tot.cached_input_tokens;
            u.cache_write = tot.cache_write_input_tokens;
            u.output = tot.output_tokens;
            u.set_context(
                last.input_tokens
                    .saturating_add(last.cache_write_input_tokens),
            );
            // `info.get("model_context_window") or st["window"]`.
            if let Some(w) = info.model_context_window.filter(|&w| w > 0) {
                u.window = Some(w);
            }
            u.requests = u.requests.saturating_add(1);
        }
        _ => {}
    }
}

/// `cwd` e inicio, da linha `session_meta`.
pub fn header(line: &[u8]) -> Header {
    let Some(d) = parse_line::<Line>(line).filter(|d| d.kind.as_deref() == Some("session_meta"))
    else {
        return Header::default();
    };
    let (cwd, ts) = d.payload.map_or((None, None), |p| (p.cwd, p.timestamp));
    Header {
        cwd,
        started_ms: ts.or(d.timestamp).as_deref().and_then(parse_iso_ms),
    }
}

/// Id do rollout pai, se a primeira linha e o `session_meta` de um subagente
/// (`thread_spawn`) e o id e um nome simples ([`valid_session_id`]).
pub fn spawn_parent(line: &[u8]) -> Option<String> {
    let d = parse_line::<Line>(line).filter(|d| d.kind.as_deref() == Some("session_meta"))?;
    let id = d
        .payload?
        .source?
        .subagent?
        .thread_spawn?
        .parent_thread_id?;
    valid_session_id(&id).then_some(id)
}

/// Id do rollout pelo nome `rollout-<inicio>-<uuid>.jsonl` (os 36 ultimos
/// caracteres antes de `.jsonl`), o mesmo do `session_meta.payload.id`.
pub fn rollout_id(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let id = rest.get(rest.len().checked_sub(36)?..)?;
    (valid_session_id(id) && id.bytes().filter(|&b| b == b'-').count() == 4).then_some(id)
}

/// Diretorios de dia onde os subagentes do rollout `parent` podem estar: o
/// do pai e os seguintes ate amanha (sessao que passa da meia-noite), no
/// maximo os [`MAX_DAYS`] ultimos. Pai fora de `root/AAAA/MM/DD`: so o
/// diretorio dele.
pub fn spawn_dirs(root: &Path, parent: &Path, now_ms: u64) -> Vec<PathBuf> {
    let Some(dir) = parent.parent() else {
        return Vec::new();
    };
    let day = dir.strip_prefix(root).ok().and_then(|rel| {
        let mut c = rel.iter().map(|c| c.to_str()?.parse::<i64>().ok());
        let (y, m, d) = (c.next()??, c.next()??, c.next()??);
        c.next().is_none().then(|| days_from_civil(y, m, d))
    });
    let Some(first) = day else {
        return vec![dir.to_path_buf()];
    };
    let last = (now_ms / 86_400_000) as i64 + 1;
    let mut out = vec![dir.to_path_buf()];
    for z in (first + 1).max(last - MAX_DAYS)..=last {
        let (y, m, d) = civil_from_days(z);
        out.push(root.join(format!("{y:04}/{m:02}/{d:02}")));
    }
    out
}

/// `rollout-*.jsonl` dos diretorios de dia entre o inicio do processo e hoje
/// (um dia de folga de cada lado: o nome do diretorio e na hora local), com o
/// mtime em ms.
pub fn candidates(root: &Path, start_ms: u64, now_ms: u64) -> Vec<(PathBuf, u64)> {
    let day = |ms: u64| (ms / 86_400_000) as i64;
    let last = day(now_ms) + 1;
    let first = (day(start_ms) - 1).max(last - MAX_DAYS);
    let mut out = Vec::new();
    for z in first..=last {
        let (y, m, d) = civil_from_days(z);
        let dir = root.join(format!("{y:04}/{m:02}/{d:02}"));
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in rd.flatten() {
            let name = ent.file_name();
            let Some(name) = name.to_str() else { continue };
            if !(name.starts_with("rollout-") && name.ends_with(".jsonl")) {
                continue;
            }
            if let Ok(meta) = ent.metadata()
                && meta.is_file()
            {
                let mtime =
                    meta.mtime().max(0) as u64 * 1000 + meta.mtime_nsec() as u64 / 1_000_000;
                out.push((ent.path(), mtime));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_count_replaces_totals_and_null_info_zeroes_them() {
        let mut u = Usage::default();
        feed(
            br#"{"type":"turn_context","payload":{"model":"gpt-x"}}"#,
            &mut u,
        );
        feed(
            br#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":40,"cache_write_input_tokens":5,"output_tokens":7},"last_token_usage":{"input_tokens":60,"cache_write_input_tokens":5},"model_context_window":258400}}}"#,
            &mut u,
        );
        assert_eq!(
            (u.input, u.cache_read, u.cache_write, u.output),
            (60, 40, 5, 7)
        );
        assert_eq!((u.context, u.window, u.requests), (65, Some(258_400), 1));
        assert_eq!(u.model.as_deref(), Some("gpt-x"));
        feed(
            br#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#,
            &mut u,
        );
        assert_eq!((u.tokens_in(), u.output, u.context), (0, 0, 0));
        assert_eq!(
            (u.context_max, u.window, u.requests),
            (65, Some(258_400), 2)
        );
    }

    #[test]
    fn tool_calls_by_call_id_and_turn_events_give_the_state() {
        let mut u = Usage::default();
        let item = |kind: &str, id: &str| {
            format!(
                "{{\"timestamp\":\"2026-10-15T04:14:10.000Z\",\"type\":\"response_item\",\
                 \"payload\":{{\"type\":\"{kind}\",\"name\":\"shell\",\"arguments\":\"{{}}\",\"call_id\":\"{id}\"}}}}"
            )
        };
        let event =
            |kind: &str| format!("{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"{kind}\"}}}}");
        feed(event("task_started").as_bytes(), &mut u);
        assert_eq!(u.busy, Some(true));
        feed(item("function_call", "c1").as_bytes(), &mut u);
        feed(item("custom_tool_call", "c2").as_bytes(), &mut u);
        feed(item("message", "c3").as_bytes(), &mut u);
        assert_eq!((u.tools.total, u.tools.open()), (2, 2));
        feed(item("function_call_output", "c1").as_bytes(), &mut u);
        assert_eq!(u.tools.open(), 1);
        feed(item("custom_tool_call_output", "c2").as_bytes(), &mut u);
        feed(event("task_complete").as_bytes(), &mut u);
        assert_eq!((u.tools.open(), u.busy), (0, Some(false)));
        feed(event("task_started").as_bytes(), &mut u);
        feed(event("turn_aborted").as_bytes(), &mut u);
        assert_eq!(u.busy, Some(false));
        // Fora de `response_item` nao e chamada.
        feed(
            br#"{"type":"event_msg","payload":{"type":"function_call","call_id":"c9"}}"#,
            &mut u,
        );
        assert_eq!(u.tools.total, 2);
    }

    #[test]
    fn header_reads_cwd_and_start_from_session_meta() {
        let h = header(
            br#"{"timestamp":"2026-10-15T04:14:06.500Z","type":"session_meta","payload":{"cwd":"/w","timestamp":"2026-10-15T04:14:06.000Z","base_instructions":{"text":"x"}}}"#,
        );
        assert_eq!(h.cwd.as_deref(), Some("/w"));
        assert_eq!(h.started_ms, parse_iso_ms("2026-10-15T04:14:06.000Z"));
        assert_eq!(
            header(br#"{"type":"turn_context","payload":{"cwd":"/w"}}"#),
            Header::default()
        );
    }

    #[test]
    fn spawn_parent_reads_only_thread_spawn_of_session_meta() {
        let kid = br#"{"type":"session_meta","payload":{"id":"k","base_instructions":{"text":"x"},"source":{"subagent":{"thread_spawn":{"parent_thread_id":"01a0-p","depth":1,"agent_role":"r"}}}}}"#;
        assert_eq!(spawn_parent(kid).as_deref(), Some("01a0-p"));
        for not_kid in [
            &br#"{"type":"session_meta","payload":{"id":"p","source":"cli"}}"#[..],
            br#"{"type":"session_meta","payload":{"source":{"subagent":"review"}}}"#,
            br#"{"type":"turn_context","payload":{"source":{"subagent":{"thread_spawn":{"parent_thread_id":"p"}}}}}"#,
            br#"{"type":"session_meta","payload":{"source":{"subagent":{"thread_spawn":{"parent_thread_id":"../x"}}}}}"#,
        ] {
            assert_eq!(spawn_parent(not_kid), None);
        }
    }

    #[test]
    fn rollout_id_and_spawn_dirs_follow_the_rollout_path() {
        let root = Path::new("/r");
        let p = root.join(
            "2026/10/15/rollout-2026-10-15T01-14-06-01a13dc4-6ff3-7fb2-a708-0d87a9e28591.jsonl",
        );
        assert_eq!(rollout_id(&p), Some("01a13dc4-6ff3-7fb2-a708-0d87a9e28591"));
        assert_eq!(rollout_id(Path::new("/r/x.jsonl")), None);
        let now = parse_iso_ms("2026-10-16T12:00:00Z").unwrap();
        let dirs: Vec<_> = spawn_dirs(root, &p, now)
            .iter()
            .map(|d| d.strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(dirs, ["2026/10/15", "2026/10/16", "2026/10/17"]);
        // Pai antigo: o dia dele e os ultimos MAX_DAYS.
        let later = parse_iso_ms("2027-01-01T00:00:00Z").unwrap();
        let dirs = spawn_dirs(root, &p, later);
        assert_eq!(dirs.len(), 1 + MAX_DAYS as usize + 1);
        assert_eq!(dirs[0], root.join("2026/10/15"));
        assert_eq!(
            spawn_dirs(root, Path::new("/x/y.jsonl"), now),
            [PathBuf::from("/x")]
        );
    }
}

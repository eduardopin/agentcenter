//! `agentcenter-cli insights [--json] [--llm URL] [--llm-timeout-ms N] [--timeout-ms N] [--proc-root PATH]`
//!
//! Os achados do motor de regras (`insights::findings`) sobre a mesma amostra do
//! `cockpit --all`, e um briefing de duas frases: o texto de regras, ou o de um
//! modelo local quando ha `--llm` e a resposta passa o guard (`briefing::brief`).
//! Aqui so a amostragem (a do `cockpit`), a escolha do endpoint e a formatacao.

use agentcenter_core::briefing::{Briefing, Endpoint, Source, brief};
use agentcenter_core::insights::{Finding, findings};
use serde_json::json;
use std::io::Write;
use std::time::Duration;

const HELP: &str = "\
usage: agentcenter-cli insights [--json] [--llm URL] [--llm-timeout-ms N]
                                [--timeout-ms N] [--proc-root PATH]

What needs attention on this machine, most urgent first: up to four findings
from fixed rules (loops, memory pressure, full contexts, idle memory, CPU
load, burn, cold caches) and a two-sentence briefing above them. Sessions are
sampled like `cockpit --all`: transcripts are read in full, and sampling
stops after the timeout.

Without --llm the briefing is the text of the rules and nothing leaves the
process. With --llm a local model words the findings; its answer is checked
(length, no invented numbers, the key facts of the first finding present) and
the rule text is shown instead when the check fails, the call fails or times
out. The line `briefing: model` or `briefing: rules` says which one you read.

The model only words the findings. It never sees transcript content or
session names (the findings carry the project directory name and a pid at
most), only loopback addresses are accepted, and it is never called by
`snapshot`, `watch` or the GUI.

  --json                 print { schema_version, taken_at_ms, briefing, findings }
  --llm URL              OpenAI-compatible server on loopback, e.g.
                         http://127.0.0.1:8080 (env AGENTCENTER_LLM)
  --llm-timeout-ms N     give the model N ms in total (default 20000)
  --timeout-ms N         stop sampling after N ms (default 15000)
  --proc-root PATH       read PATH instead of /proc (fixtures)

A URL that is not http://127.0.0.1, http://localhost or http://[::1] with a
port is a usage error (exit code 2).";

/// Pelo ambiente, quando nao ha `--llm`.
pub const LLM_ENV: &str = "AGENTCENTER_LLM";

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub json: bool,
    pub llm: Option<String>,
    pub llm_timeout_ms: u64,
    pub timeout_ms: u64,
    pub proc_root: Option<String>,
}

fn millis(flag: &str, v: &str) -> Result<u64, String> {
    v.parse()
        .map_err(|_| format!("{flag}: `{v}` is not a number of milliseconds"))
}

/// `Ok(None)`: pediu `--help`.
pub fn parse(args: &[String]) -> Result<Option<Options>, String> {
    let mut o = Options {
        json: false,
        llm: None,
        llm_timeout_ms: 20_000,
        timeout_ms: 15_000,
        proc_root: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "--json" => o.json = true,
            "--llm" => o.llm = Some(value("--llm")?),
            "--llm-timeout-ms" => {
                o.llm_timeout_ms = millis("--llm-timeout-ms", &value("--llm-timeout-ms")?)?;
            }
            "--timeout-ms" => o.timeout_ms = millis("--timeout-ms", &value("--timeout-ms")?)?,
            "--proc-root" => o.proc_root = Some(value("--proc-root")?),
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("insights: unknown option `{other}`\n\n{HELP}")),
        }
    }
    Ok(Some(o))
}

/// O endpoint de `--llm` ou do ambiente; so loopback, senao erro de uso.
fn endpoint(opts: &Options, env: Option<String>) -> Result<Option<Endpoint>, String> {
    match opts.llm.clone().or(env).filter(|u| !u.trim().is_empty()) {
        None => Ok(None),
        Some(url) => Endpoint::parse(url.trim()).map(Some).map_err(|e| {
            format!("--llm: {e}; only loopback addresses are accepted (http://127.0.0.1:PORT)")
        }),
    }
}

/// Texto simples, sem cor: o briefing, de onde veio, e os achados numerados.
pub fn render(b: &Briefing, fs: &[Finding]) -> String {
    let mut out = format!("{}\n", b.text);
    let source = match b.source {
        Source::Rules => "rules",
        Source::Model => "model",
    };
    out.push_str(&format!("briefing: {source}"));
    if let Some(why) = &b.rejected {
        out.push_str(&format!(" (model answer refused: {why})"));
    }
    out.push_str("\n\n");
    for (i, f) in fs.iter().enumerate() {
        out.push_str(&format!("{}. [{}] {}\n", i + 1, f.severity, f.text));
    }
    out
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let Some(opts) = parse(&args)? else {
        println!("{HELP}");
        return Ok(());
    };
    // Antes de amostrar: um endereco recusado nao custa a amostragem.
    let endpoint = endpoint(&opts, std::env::var(LLM_ENV).ok())?;
    let snap = super::cockpit::sample_all(opts.proc_root.as_deref(), opts.timeout_ms)?;
    let fs = findings(&snap);
    let briefing = brief(
        &fs,
        endpoint.as_ref(),
        Duration::from_millis(opts.llm_timeout_ms),
    );

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let res = if opts.json {
        let doc = json!({
            "schema_version": 1,
            "taken_at_ms": snap.taken_at_ms,
            "briefing": briefing,
            "findings": fs,
        });
        serde_json::to_writer(&mut out, &doc)
            .map_err(|e| e.to_string())
            .and_then(|()| writeln!(out).map_err(|e| e.to_string()))
    } else {
        out.write_all(render(&briefing, &fs).as_bytes())
            .map_err(|e| e.to_string())
    };
    // Pipe fechado (`| head`) nao e erro.
    match res.and_then(|()| out.flush().map_err(|e| e.to_string())) {
        Err(e) if e.contains("Broken pipe") => Ok(()),
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_the_options_and_their_defaults() {
        let o = parse(&args(&[])).unwrap().unwrap();
        assert_eq!(o.llm, None);
        assert_eq!((o.llm_timeout_ms, o.timeout_ms), (20_000, 15_000));
        let o = parse(&args(&[
            "--json",
            "--llm=http://127.0.0.1:1",
            "--llm-timeout-ms",
            "5",
        ]))
        .unwrap()
        .unwrap();
        assert!(o.json);
        assert_eq!(o.llm.as_deref(), Some("http://127.0.0.1:1"));
        assert_eq!(o.llm_timeout_ms, 5);
        assert!(parse(&args(&["--help"])).unwrap().is_none());
        assert!(parse(&args(&["--timeout-ms", "x"])).is_err());
    }

    #[test]
    fn the_flag_wins_over_the_environment_and_only_loopback_passes() {
        let o = parse(&args(&["--llm", "http://127.0.0.1:1"]))
            .unwrap()
            .unwrap();
        assert!(
            endpoint(&o, Some("http://10.0.0.5:1".into()))
                .unwrap()
                .is_some()
        );
        let o = parse(&args(&[])).unwrap().unwrap();
        assert!(endpoint(&o, None).unwrap().is_none());
        assert!(endpoint(&o, Some(" ".into())).unwrap().is_none());
        let e = endpoint(&o, Some("http://10.0.0.5:1".into())).unwrap_err();
        assert!(e.contains("loopback"), "{e}");
    }
}

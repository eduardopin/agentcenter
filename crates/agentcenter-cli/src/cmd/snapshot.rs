//! `agentcenter-cli snapshot [--json] [--interval-ms N] [--proc-root PATH]`

use agentcenter_core::{ProcRoot, READ_ALL, Registry, Sampler, SamplerConfig};
use std::io::Write;
use std::time::Duration;

const HELP: &str = "\
usage: agentcenter-cli snapshot [--json] [--interval-ms N] [--proc-root PATH]

Takes two samples N ms apart and prints one row per AI session. Transcripts
are read in full on the first sample, so tokens, tool calls and the cockpit
are filled on the second (watch and the GUI read at most 1 MiB per tick).

  --json            print the snapshot as JSON instead of a table
  --interval-ms N   time between the two samples (default 1000)
  --proc-root PATH  read PATH instead of /proc (fixtures)

The first line sums the agent sessions (herdr left out) by agent and by the
state the agent or its herdr pane reports (busy, blocked), and counts the
active processes: session children in state R or D or above 1 % CPU.

In the table `-` is a missing value (no previous sample, unreadable) and a
trailing `*` marks a sum that leaves out some processes (JSON: null and
`partial`). Control characters are shown escaped (`\\x1b`, `\\n`).

env AGENTCENTER_EXTRA_AGENTS=comm1,comm2 adds agents detected by comm.";

pub use super::Options;

pub fn parse(args: &[String]) -> Result<Option<Options>, String> {
    super::parse(args, "snapshot", HELP)
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let Some(opts) = parse(&args)? else {
        println!("{HELP}");
        return Ok(());
    };
    let root = opts
        .proc_root
        .as_deref()
        .map_or_else(ProcRoot::default, ProcRoot::new);
    let mut sampler = Sampler::with_config(Registry::from_env(), one_shot_config());
    let err = |e| format!("reading {}: {e}", root.path().display());
    sampler.tick(&root).map_err(err)?;
    std::thread::sleep(Duration::from_millis(opts.interval_ms));
    let snap = sampler.tick(&root).map_err(err)?;

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let res = if opts.json {
        serde_json::to_writer(&mut out, &snap)
            .map_err(|e| e.to_string())
            .and_then(|()| writeln!(out).map_err(|e| e.to_string()))
    } else {
        let home = std::env::var("HOME").ok();
        out.write_all(crate::table::render(&snap, home.as_deref()).as_bytes())
            .map_err(|e| e.to_string())
    };
    // Pipe fechado (`| head`) nao e erro.
    match res.and_then(|()| out.flush().map_err(|e| e.to_string())) {
        Err(e) if e.contains("Broken pipe") => Ok(()),
        r => r,
    }
}

/// Um tiro so: le os transcripts inteiros ja no primeiro tick, para que tokens,
/// tool calls e cockpit saiam no segundo. O `watch` e a GUI seguem com o teto
/// padrao por tick.
pub fn one_shot_config() -> SamplerConfig {
    SamplerConfig {
        transcript_read_budget: READ_ALL,
        ..SamplerConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_defaults_and_flags() {
        let o = parse(&args(&[])).unwrap().unwrap();
        assert_eq!(
            o,
            Options {
                json: false,
                interval_ms: 1000,
                proc_root: None
            }
        );
        let o = parse(&args(&["--json", "--interval-ms", "250", "--proc-root=/x"]))
            .unwrap()
            .unwrap();
        assert_eq!(
            o,
            Options {
                json: true,
                interval_ms: 250,
                proc_root: Some("/x".into())
            }
        );
        assert!(parse(&args(&["--interval-ms", "abc"])).is_err());
        assert!(parse(&args(&["--bogus"])).is_err());
        assert_eq!(parse(&args(&["--help"])).unwrap(), None);
    }

    #[test]
    fn one_shot_reads_transcripts_in_full_and_keeps_the_other_defaults() {
        let c = one_shot_config();
        assert_eq!(c.transcript_read_budget, READ_ALL);
        assert_ne!(SamplerConfig::default().transcript_read_budget, READ_ALL);
        assert_eq!(
            SamplerConfig {
                transcript_read_budget: SamplerConfig::default().transcript_read_budget,
                ..c
            },
            SamplerConfig::default()
        );
    }
}

//! `agentcenter-cli watch [--json] [--interval-ms N] [--proc-root PATH]`

use agentcenter_core::procfs::ReadError;
use agentcenter_core::{Frame, ProcRoot, Registry, Sampler, SamplerConfig, Snapshot};
use std::io::{ErrorKind, IsTerminal, Write};
use std::time::Duration;

const HELP: &str = "\
usage: agentcenter-cli watch [--json] [--interval-ms N] [--proc-root PATH]

Samples every N ms until interrupted and prints one snapshot per tick: the
table redrawn in place (on a terminal), or one JSON object per line (NDJSON).

  --json            NDJSON: one snapshot per line, same format as `snapshot --json`
  --interval-ms N   time between ticks (default 1000)
  --proc-root PATH  read PATH instead of /proc (fixtures)

The first tick has no rates (`-`, null). PSS and swap of a session are re-read
every 5 s if its memory moved and at most every 300 s otherwise, so they can
lag the CPU and disk columns; uid and context switches lag up to 5 s. A quiet
session (CPU under 1 %, no disk or stall) is re-read only every 5 s: between
reads it shows the mean of its last window, and new work takes up to 5 s to
show. The agent's cmdline and cwd can be up to 5 s stale. See
`snapshot --help` for the table legend.

env AGENTCENTER_EXTRA_AGENTS=comm1,comm2 adds agents detected by comm.";

/// Cursor no canto e tela limpa: redesenho sem crate de TUI.
const CLEAR: &str = "\x1b[H\x1b[2J";

/// Um frame como sai no stdout. Tabela fora de terminal: separada por linha vazia.
fn render(snap: &Snapshot, json: bool, tty: bool, home: Option<&str>, out: &mut Vec<u8>) {
    out.clear();
    if json {
        // Snapshot so tem tipos que o serde_json serializa sem erro.
        serde_json::to_writer(&mut *out, snap).expect("snapshot serializes");
        out.push(b'\n');
        return;
    }
    if tty {
        out.extend_from_slice(CLEAR.as_bytes());
    }
    out.extend_from_slice(crate::table::render(snap, home).as_bytes());
    if !tty {
        out.push(b'\n');
    }
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let Some(opts) = super::parse(&args, "watch", HELP)? else {
        println!("{HELP}");
        return Ok(());
    };
    if opts.interval_ms == 0 {
        return Err("watch: --interval-ms must be at least 1".into());
    }
    let root = opts
        .proc_root
        .as_deref()
        .map_or_else(ProcRoot::default, ProcRoot::new);
    let config = SamplerConfig {
        interval: Duration::from_millis(opts.interval_ms),
        ..SamplerConfig::default()
    };
    let sampler = Sampler::with_config(Registry::from_env(), config);
    let path = root.path().display().to_string();
    let handle = sampler.spawn(root, false);

    let stdout = std::io::stdout();
    let tty = stdout.is_terminal();
    let home = std::env::var("HOME").ok();
    let mut out = stdout.lock();
    stream(
        || handle.recv(),
        opts.json,
        tty,
        home.as_deref(),
        &path,
        &mut out,
    )
}

/// Escreve um frame por tick ate `next` acabar. `watch` so termina por sinal
/// ou pipe fechado: a thread do sampler so para sozinha se cair (panico), entao
/// fonte vazia e erro, nao saida normal.
fn stream(
    mut next: impl FnMut() -> Option<Result<Frame, ReadError>>,
    json: bool,
    tty: bool,
    home: Option<&str>,
    path: &str,
    out: &mut impl Write,
) -> Result<(), String> {
    let mut buf = Vec::with_capacity(64 * 1024);
    while let Some(frame) = next() {
        let frame = frame.map_err(|e| format!("reading {path}: {e}"))?;
        render(&frame.snapshot, json, tty, home, &mut buf);
        match out.write_all(&buf).and_then(|()| out.flush()) {
            // Pipe fechado (`| head`) e o jeito normal de parar.
            Err(e) if e.kind() == ErrorKind::BrokenPipe => return Ok(()),
            Err(e) => return Err(e.to_string()),
            Ok(()) => {}
        }
    }
    Err("watch: the sampler thread stopped unexpectedly".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};

    fn snap() -> (Snapshot, TempDir) {
        let dir = TempDir::new("ac-watch").unwrap();
        let fp = FakeProc::create(dir.path()).unwrap();
        fp.add(&FakeProcess::new(20, 1, "claude")).unwrap();
        let s = Sampler::new(Registry::default())
            .tick(&fp.proc_root())
            .unwrap();
        (s, dir)
    }

    #[test]
    fn json_frame_is_one_line_of_the_snapshot_contract() {
        let (s, _d) = snap();
        let mut out = Vec::new();
        render(&s, true, true, None, &mut out);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches('\n').count(), 1);
        assert!(text.ends_with('\n') && !text.contains('\x1b'));
        let back: Snapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn table_frame_clears_only_on_a_terminal() {
        let (s, _d) = snap();
        let mut out = Vec::new();
        render(&s, false, true, None, &mut out);
        let tty = String::from_utf8(out.clone()).unwrap();
        assert!(tty.starts_with(CLEAR), "{tty:?}");
        assert_eq!(tty.matches('\x1b').count(), 2, "so o CLEAR");
        render(&s, false, false, None, &mut out);
        let pipe = String::from_utf8(out).unwrap();
        assert!(!pipe.contains('\x1b'));
        assert!(pipe.ends_with("\n\n"), "frames separados por linha vazia");
        // Resumo no topo, depois o cabecalho da tabela.
        let mut lines = pipe.lines();
        let top = lines.next().unwrap();
        assert!(top.starts_with("1 session (claude 1) | "), "{pipe}");
        assert!(lines.next().unwrap().starts_with("AGENT"), "{pipe}");
    }

    #[test]
    fn sampler_thread_dying_is_an_error_not_a_clean_exit() {
        let (s, _d) = snap();
        let frame = Frame {
            snapshot: std::sync::Arc::new(s),
            history: None,
        };
        // Mesma ponta que o SamplerHandle: a thread entrega um frame e entra em
        // panico; o canal fecha e `recv` devolve None.
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let t = std::thread::spawn(move || {
            tx.send(Ok(frame)).unwrap();
            panic!("sampler caiu (teste)");
        });
        let mut out = Vec::new();
        let err = stream(|| rx.recv().ok(), true, false, None, "/proc", &mut out).unwrap_err();
        assert!(t.join().is_err());
        assert!(err.contains("sampler thread stopped"), "{err}");
        assert_eq!(
            out.iter().filter(|&&b| b == b'\n').count(),
            1,
            "o frame saiu"
        );
    }

    #[test]
    fn closed_pipe_ends_the_stream_cleanly() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (s, _d) = snap();
        let snapshot = std::sync::Arc::new(s);
        let next = || {
            Some(Ok(Frame {
                snapshot: snapshot.clone(),
                history: None,
            }))
        };
        assert_eq!(
            stream(next, true, false, None, "/proc", &mut Closed),
            Ok(())
        );
    }

    #[test]
    fn help_tells_the_5_s_lag_of_quiet_sessions_and_cmdline_cwd() {
        let one_line = HELP.split_whitespace().collect::<Vec<_>>().join(" ");
        for phrase in [
            "A quiet session (CPU under 1 %, no disk or stall) is re-read only every 5 s",
            "shows the mean of its last window, and new work takes up to 5 s to show",
            "The agent's cmdline and cwd can be up to 5 s stale",
        ] {
            assert!(one_line.contains(phrase), "{phrase:?} not in help");
        }
    }

    #[test]
    fn zero_interval_is_rejected() {
        let err = run(vec!["--interval-ms".into(), "0".into()]).unwrap_err();
        assert!(err.contains("at least 1"), "{err}");
    }
}

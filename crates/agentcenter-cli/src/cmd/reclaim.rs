//! `agentcenter-cli reclaim [--json] [--window-s N] [--proc-root PATH]`
//!
//! Somente leitura: observa a janela, lista e sugere o `signal`; nunca envia sinal.

use crate::table::{human_bytes, sanitize};
use agentcenter_core::reclaim::{self, DEFAULT_WINDOW_S};
use agentcenter_core::{Candidate, CpuWindow, IdleBy, ProcRoot, Reclaim, Sampler};
use std::fmt::Write as _;
use std::io::Write;
use std::time::Duration;

const HELP: &str = "\
usage: agentcenter-cli reclaim [--json] [--window-s N] [--proc-root PATH]

Watches every AI session for N seconds (one sample per second) and lists the
idle ones, largest PSS + SwapPss first, with the memory they would free and
the command that would end each one. Read only: nothing is signalled.

  --json            print {window_s, total_bytes, sessions[]} as JSON
  --window-s N      seconds to watch (default 10, at least 1)
  --proc-root PATH  read PATH instead of /proc (fixtures)

A session is idle when its overlay status is `idle` (Claude, herdr), or, when
no overlay gives a status, when its CPU stayed under 1% of a core in every
sample of the window. A session that was not seen with a complete CPU sum in
every sample (new, or a new process in it) is not idle. `busy` and `blocked`
sessions are never listed. Also left out: the multiplexer (herdr server), the
session this command runs in, and sessions whose PSS cannot be read (another
user's). A quiet session is re-read every 5 s, so its CPU is the mean of the
last 5 s. In the table `*` marks a memory sum that leaves out some processes.

env AGENTCENTER_EXTRA_AGENTS=comm1,comm2 adds agents detected by comm.";

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub json: bool,
    pub window_s: u64,
    pub proc_root: Option<String>,
}

/// `Ok(None)`: pediu `--help`.
pub fn parse(args: &[String]) -> Result<Option<Options>, String> {
    let mut o = Options {
        json: false,
        window_s: DEFAULT_WINDOW_S,
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
            "--window-s" => {
                let v = value("--window-s")?;
                o.window_s =
                    v.parse().ok().filter(|&n| n >= 1).ok_or_else(|| {
                        format!("--window-s: `{v}` is not a number of seconds >= 1")
                    })?;
            }
            "--proc-root" => o.proc_root = Some(value("--proc-root")?),
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("reclaim: unknown option `{other}`\n\n{HELP}")),
        }
    }
    Ok(Some(o))
}

/// O comando que terminaria a sessao. So texto: `reclaim` nao o executa.
pub fn suggested_command(pid: u32) -> String {
    format!("agentcenter-cli signal term --session {pid}")
}

/// JSON: o [`Reclaim`] do core, a janela e o comando sugerido por linha.
pub fn to_json(r: &Reclaim, window_s: u64) -> serde_json::Value {
    let mut v = serde_json::to_value(r).expect("reclaim serializes");
    v["window_s"] = window_s.into();
    if let Some(rows) = v["sessions"].as_array_mut() {
        for (row, c) in rows.iter_mut().zip(&r.sessions) {
            row["command"] = suggested_command(c.pid).into();
        }
    }
    v
}

fn row(c: &Candidate) -> [String; 10] {
    let star = if c.memory_partial { "*" } else { "" };
    let o = c.overlay.as_ref();
    [
        c.agent.clone(),
        c.pid.to_string(),
        match c.idle_by {
            IdleBy::Overlay => "status",
            IdleBy::Cpu => "cpu",
        }
        .into(),
        c.max_cpu_pct
            .map_or_else(|| "-".into(), |v| format!("{v:.1}")),
        format!("{}{star}", human_bytes(c.pss_bytes as f64)),
        c.swap_pss_bytes.map_or_else(
            || "-".into(),
            |b| format!("{}{star}", human_bytes(b as f64)),
        ),
        format!("{}{star}", human_bytes(c.memory_bytes() as f64)),
        o.and_then(|o| o.name.clone()).unwrap_or_else(|| "-".into()),
        suggested_command(c.pid),
        c.cwd.clone().unwrap_or_else(|| "-".into()),
    ]
}

/// Tabela: cabecalho, uma linha por sessao (ordem do core) e o total.
/// Toda celula passa por [`sanitize`]: agente, nome e cwd vem de fora.
pub fn render(r: &Reclaim, window_s: u64) -> String {
    let mut out = String::new();
    let n = r.sessions.len();
    let _ = writeln!(
        out,
        "reclaimable: {} in {n} idle session{} (watched {window_s} s; nothing was signalled)",
        human_bytes(r.total_bytes as f64),
        if n == 1 { "" } else { "s" },
    );
    if n == 0 {
        return out;
    }
    let head = [
        "AGENT", "PID", "IDLE", "MAXCPU%", "PSS", "SWAP", "TOTAL", "NAME", "COMMAND", "CWD",
    ];
    let mut rows: Vec<[String; 10]> = vec![head.map(String::from)];
    rows.extend(r.sessions.iter().map(row));
    for r in &mut rows {
        for c in r.iter_mut() {
            if let std::borrow::Cow::Owned(clean) = sanitize(c) {
                *c = clean;
            }
        }
    }
    let mut widths = [0usize; 10];
    for r in &rows {
        for (w, c) in widths.iter_mut().zip(r) {
            *w = (*w).max(c.chars().count());
        }
    }
    for r in &rows {
        let mut line = String::new();
        for (i, c) in r.iter().enumerate() {
            // Numeros a direita (PID..TOTAL); ultima coluna sem preenchimento.
            if i == 9 {
                line.push_str(c);
            } else if (1..=6).contains(&i) && i != 2 {
                let _ = write!(line, "{c:>w$}  ", w = widths[i]);
            } else {
                let _ = write!(line, "{c:<w$}  ", w = widths[i]);
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
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
    let mut sampler = Sampler::default();
    let err = |e| format!("reading {}: {e}", root.path().display());
    let mut window = CpuWindow::new();
    let mut snap = sampler.tick(&root).map_err(err)?;
    for _ in 0..opts.window_s {
        std::thread::sleep(Duration::from_secs(1));
        snap = sampler.tick(&root).map_err(err)?;
        window.observe(&snap);
    }
    let r = reclaim::reclaim(&snap, &window, Some(std::process::id()));

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let res = if opts.json {
        serde_json::to_writer(&mut out, &to_json(&r, opts.window_s))
            .map_err(|e| e.to_string())
            .and_then(|()| writeln!(out).map_err(|e| e.to_string()))
    } else {
        out.write_all(render(&r, opts.window_s).as_bytes())
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
    use agentcenter_core::overlay::{OverlayStatus, SessionOverlay};

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    fn cand(pid: u32, agent: &str, pss: u64, swap: Option<u64>) -> Candidate {
        Candidate {
            pid,
            agent: agent.into(),
            cwd: Some("/w".into()),
            overlay: None,
            idle_by: IdleBy::Cpu,
            max_cpu_pct: Some(0.25),
            pss_bytes: pss,
            swap_pss_bytes: swap,
            memory_partial: false,
        }
    }

    fn reclaim_of(sessions: Vec<Candidate>) -> Reclaim {
        Reclaim {
            total_bytes: sessions.iter().map(Candidate::memory_bytes).sum(),
            sessions,
        }
    }

    #[test]
    fn parse_defaults_and_flags() {
        let o = parse(&args(&[])).unwrap().unwrap();
        assert_eq!(
            o,
            Options {
                json: false,
                window_s: 10,
                proc_root: None
            }
        );
        let o = parse(&args(&["--json", "--window-s", "3", "--proc-root=/x"]))
            .unwrap()
            .unwrap();
        assert_eq!(
            o,
            Options {
                json: true,
                window_s: 3,
                proc_root: Some("/x".into())
            }
        );
        assert_eq!(
            parse(&args(&["--window-s=1"])).unwrap().unwrap().window_s,
            1
        );
        for bad in [
            &["--window-s", "0"][..],
            &["--window-s", "x"],
            &["--window-s"],
            &["--bogus"],
            &["term"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
        assert_eq!(parse(&args(&["--help"])).unwrap(), None);
    }

    #[test]
    fn json_has_total_window_and_command_per_row() {
        let mut a = cand(10, "claude", 2 << 30, Some(1 << 20));
        a.overlay = Some(SessionOverlay {
            status: Some(OverlayStatus::Idle),
            ..Default::default()
        });
        a.idle_by = IdleBy::Overlay;
        let r = reclaim_of(vec![a, cand(20, "codex", 1 << 30, None)]);
        let v = to_json(&r, 7);
        assert_eq!(v["window_s"], 7);
        assert_eq!(v["total_bytes"], (2u64 << 30) + (1 << 20) + (1 << 30));
        let rows = v["sessions"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["pid"], 10);
        assert_eq!(rows[0]["idle_by"], "overlay");
        assert_eq!(rows[0]["overlay"]["status"], "idle");
        assert_eq!(
            rows[0]["command"],
            "agentcenter-cli signal term --session 10"
        );
        assert_eq!(rows[1]["swap_pss_bytes"], serde_json::Value::Null);
        assert_eq!(
            rows[1]["command"],
            "agentcenter-cli signal term --session 20"
        );
    }

    #[test]
    fn table_lists_rows_in_order_with_total_and_command() {
        let mut b = cand(20, "codex", 1 << 30, None);
        b.memory_partial = true;
        let r = reclaim_of(vec![cand(10, "claude", 2 << 30, Some(1 << 30)), b]);
        let out = render(&r, 10);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0],
            "reclaimable: 4.0G in 2 idle sessions (watched 10 s; nothing was signalled)"
        );
        assert!(lines[1].starts_with("AGENT "), "{out}");
        let row: Vec<&str> = lines[2].split_whitespace().collect();
        assert_eq!(
            row,
            [
                "claude",
                "10",
                "cpu",
                "0.2",
                "2.0G",
                "1.0G",
                "3.0G",
                "-",
                "agentcenter-cli",
                "signal",
                "term",
                "--session",
                "10",
                "/w"
            ]
        );
        let row: Vec<&str> = lines[3].split_whitespace().collect();
        assert_eq!(
            &row[..7],
            ["codex", "20", "cpu", "0.2", "1.0G*", "-", "1.0G*"]
        );
        assert_eq!(lines.len(), 4, "{out}");

        let empty = render(&reclaim_of(vec![]), 3);
        assert_eq!(
            empty,
            "reclaimable: 0B in 0 idle sessions (watched 3 s; nothing was signalled)\n"
        );
    }

    #[test]
    fn escape_sequences_from_agent_name_and_cwd_never_reach_the_terminal() {
        let evil = "x\x1b[2J\nfake\u{202e}";
        let mut c = cand(10, evil, 1 << 20, Some(0));
        c.cwd = Some(evil.into());
        c.overlay = Some(SessionOverlay {
            name: Some(evil.into()),
            ..Default::default()
        });
        let out = render(&reclaim_of(vec![c]), 1);
        assert!(
            !out.chars().any(|c| c.is_control() && c != '\n'),
            "controle cru: {out:?}"
        );
        assert!(!out.contains('\u{202e}'), "{out:?}");
        assert_eq!(out.matches(r"x\x1b[2J\nfake\u{202e}").count(), 3, "{out}");
        assert_eq!(out.lines().count(), 3, "{out}");
    }
}

//! `agentcenter-cli signal <stop|cont|term|kill> (--session PID | --pid PID) [--yes]`

use crate::table::{human_bytes, sanitize};
use agentcenter_core::actions::{self, Action, Delivery, Guard, Outcome, Plan, Refusal, Target};
use agentcenter_core::{ProcRoot, Registry};
use std::fmt::Write as _;
use std::io::{BufRead, IsTerminal, Write};

const HELP: &str = "\
usage: agentcenter-cli signal <stop|cont|term|kill> (--session PID | --pid PID) [--yes]

Sends a signal to a whole session or to one process.

  stop   SIGSTOP, suspend (root first, so a parent cannot respawn a child)
  cont   SIGCONT, resume (leaves first)
  term   SIGTERM, ask to exit (root first)
  kill   SIGKILL, kill (root first)

  --session PID  the agent PID and every process of its session
  --pid PID      only this process
  --yes          do not ask; without it the processes are listed and a
                 terminal is asked to confirm (no terminal: nothing is sent)

Only processes of your own user are signalled, never this process, its
ancestors, pid 1 or the herdr server. Each pid is checked again right before
its signal; a pid reused by another process is skipped.

env AGENTCENTER_EXTRA_AGENTS=comm1,comm2 adds agents detected by comm.";

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub action: Action,
    pub target: Target,
    pub yes: bool,
}

/// `Ok(None)`: pediu `--help`.
pub fn parse(args: &[String]) -> Result<Option<Options>, String> {
    let mut action = None;
    let mut target = None;
    let mut yes = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut pid = |name: &str| -> Result<u32, String> {
            let v = inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{name} needs a pid"))?;
            v.parse().map_err(|_| format!("{name}: `{v}` is not a pid"))
        };
        let t = match flag {
            "--session" => Target::Session(pid("--session")?),
            "--pid" => Target::Process(pid("--pid")?),
            "-y" | "--yes" => {
                yes = true;
                continue;
            }
            "-h" | "--help" => return Ok(None),
            other if !other.starts_with('-') && action.is_none() => {
                action = Some(Action::parse(other).ok_or_else(|| {
                    format!("signal: unknown action `{other}` (stop, cont, term or kill)")
                })?);
                continue;
            }
            other => return Err(format!("signal: unexpected `{other}`\n\n{HELP}")),
        };
        if target.replace(t).is_some() {
            return Err("signal: give one of --session or --pid, once".into());
        }
    }
    let action = action.ok_or_else(|| format!("signal: missing action\n\n{HELP}"))?;
    let target = target.ok_or_else(|| format!("signal: missing --session or --pid\n\n{HELP}"))?;
    Ok(Some(Options {
        action,
        target,
        yes,
    }))
}

/// Lista o que o plano atinge: pid, comm e PSS + swap de cada alvo.
pub fn render_plan(plan: &Plan) -> String {
    let mut s = String::new();
    let (kind, pid) = match plan.target {
        Target::Session(p) => ("session", p),
        Target::Process(p) => ("process", p),
    };
    let comm = plan
        .victims
        .first()
        .map(|v| sanitize(&v.comm).into_owned())
        .unwrap_or_default();
    let n = plan.victims.len();
    let _ = writeln!(
        s,
        "{} to {kind} {pid} ({comm}): {n} process{}",
        plan.action.signal_name(),
        if n == 1 { "" } else { "es" }
    );
    let _ = writeln!(s, "{:>8}  {:<16} {:>10}", "PID", "COMM", "PSS+SWAP");
    for v in &plan.victims {
        let mem = v
            .memory_bytes()
            .map_or_else(|| "-".to_string(), |b| human_bytes(b as f64));
        let _ = writeln!(s, "{:>8}  {:<16} {:>10}", v.pid, sanitize(&v.comm), mem);
    }
    if !plan.skipped.is_empty() {
        let _ = writeln!(s, "not signalled:");
        for (pid, why) in &plan.skipped {
            let _ = writeln!(s, "{pid:>8}  {}", sanitize(&why.to_string()));
        }
    }
    s
}

/// Mensagem de recusa. Escapada inteira: `comm`, `agent` e o erro vem de
/// `/proc` e qualquer processo controla o proprio `comm` (ESC, `\n`).
fn refusal_message(e: &Refusal) -> String {
    format!("refused: {}", sanitize(&e.to_string()))
}

fn render_outcome(action: Action, o: &Outcome) -> String {
    let pid = o.pid;
    match &o.delivery {
        Delivery::Sent => format!("sent {} to {pid}", action.signal_name()),
        Delivery::Gone => format!("skipped {pid}: already exited"),
        Delivery::Reused => format!("skipped {pid}: pid now belongs to another process"),
        Delivery::Failed(e) => format!("failed {pid}: {}", sanitize(e)),
    }
}

/// Pergunta no terminal. `false` em qualquer coisa que nao seja `y`/`yes`.
fn confirm(plan: &Plan) -> Result<bool, String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(
            "stdin is not a terminal, cannot ask for confirmation; nothing sent \
             (pass --yes to send without asking)"
                .into(),
        );
    }
    let mut err = std::io::stderr();
    let _ = write!(
        err,
        "send {} to {} process(es)? [y/N] ",
        plan.action.signal_name(),
        plan.victims.len()
    );
    let _ = err.flush();
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}

pub fn run(args: Vec<String>) -> Result<(), String> {
    let Some(opts) = parse(&args)? else {
        println!("{HELP}");
        return Ok(());
    };
    let root = ProcRoot::default();
    let guard = Guard::current(&root);
    let plan = actions::plan(
        &root,
        &Registry::from_env(),
        &guard,
        opts.action,
        opts.target,
    )
    .map_err(|e| refusal_message(&e))?;
    print!("{}", render_plan(&plan));
    if !opts.yes && !confirm(&plan)? {
        return Err("not confirmed; nothing sent".into());
    }
    let outcomes = plan.execute(&root);
    let mut failed = 0;
    for o in &outcomes {
        println!("{}", render_outcome(opts.action, o));
        failed += usize::from(matches!(o.delivery, Delivery::Failed(_)));
    }
    if failed > 0 {
        return Err(format!("{failed} of {} signals failed", outcomes.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::actions::Victim;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_action_target_and_yes() {
        let o = parse(&args(&["term", "--session", "42"])).unwrap().unwrap();
        assert_eq!(
            o,
            Options {
                action: Action::Term,
                target: Target::Session(42),
                yes: false
            }
        );
        let o = parse(&args(&["--pid=7", "kill", "--yes"]))
            .unwrap()
            .unwrap();
        assert_eq!(
            o,
            Options {
                action: Action::Kill,
                target: Target::Process(7),
                yes: true
            }
        );
        assert_eq!(parse(&args(&["--help"])).unwrap(), None);
    }

    #[test]
    fn parse_rejects_bad_input() {
        for bad in [
            &["term"][..],
            &["--pid", "3"],
            &["hup", "--pid", "3"],
            &["term", "--pid", "x"],
            &["term", "--pid"],
            &["term", "--pid", "3", "--session", "4"],
            &["term", "stop", "--pid", "3"],
            &["term", "--pid", "3", "--bogus"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn render_plan_lists_pid_comm_memory_and_skipped() {
        let v = |pid, comm: &str, pss: Option<u64>| Victim {
            pid,
            ppid: 1,
            comm: comm.into(),
            starttime: 1,
            pss_bytes: pss,
            swap_pss_bytes: pss.map(|_| 1 << 20),
        };
        let plan = Plan {
            action: Action::Stop,
            target: Target::Session(10),
            victims: vec![v(10, "claude", Some(3 << 20)), v(11, "no\x1bde", None)],
            skipped: vec![(
                12,
                Refusal::OtherUser {
                    pid: 12,
                    uid: 0,
                    mine: 1000,
                },
            )],
        };
        let out = render_plan(&plan);
        assert!(out.starts_with("SIGSTOP to session 10 (claude): 2 processes\n"));
        let row = out.lines().find(|l| l.contains("claude ")).unwrap();
        assert!(row.contains(&human_bytes((4u64 << 20) as f64)), "{row}");
        let row = out
            .lines()
            .find(|l| l.trim_start().starts_with("11"))
            .unwrap();
        assert!(
            row.contains("no\\x1bde") && row.trim_end().ends_with('-'),
            "{row}"
        );
        assert!(out.contains("not signalled:\n      12  pid 12 belongs to uid 0"));
    }

    #[test]
    fn escape_sequences_from_comm_never_reach_the_terminal() {
        let evil = "x\x1b[2J\nfake";
        let why = Refusal::Ancestor {
            pid: 5,
            comm: evil.into(),
        };
        let plan = Plan {
            action: Action::Term,
            target: Target::Session(4),
            victims: vec![Victim {
                pid: 4,
                ppid: 1,
                comm: evil.into(),
                starttime: 1,
                pss_bytes: None,
                swap_pss_bytes: None,
            }],
            skipped: vec![(5, why.clone())],
        };
        let multiplexer = Refusal::Multiplexer {
            pid: 6,
            agent: evil.into(),
        };
        let failed = Outcome {
            pid: 7,
            delivery: Delivery::Failed(evil.into()),
        };
        for out in [
            render_plan(&plan),
            refusal_message(&why),
            refusal_message(&multiplexer),
            render_outcome(Action::Term, &failed),
        ] {
            assert!(!out.contains('\x1b'), "ESC cru: {out:?}");
            assert!(out.contains("x\\x1b[2J"), "{out:?}");
            // So as quebras de linha do proprio formato.
            assert!(!out.contains("\nfake"), "{out:?}");
        }
    }
}

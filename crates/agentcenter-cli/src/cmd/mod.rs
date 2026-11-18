//! Um modulo por subcomando.

pub mod cockpit;
pub mod insights;
pub mod reclaim;
pub mod signal;
pub mod snapshot;
pub mod watch;

/// One subcommand: the single place the dispatch, the usage text and the
/// generated docs (`docs/CLI.md`, `man/agentcenter-cli.1`) all read from.
pub struct Command {
    pub name: &'static str,
    /// One line for the top-level usage and the docs.
    pub summary: &'static str,
    /// The command's `--help` text: usage, flags, notes.
    pub help: &'static str,
    pub run: fn(Vec<String>) -> Result<(), String>,
}

/// Every subcommand, in the order the usage lists them. A new subcommand is
/// added here or it does not exist: `main` dispatches through this table.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "snapshot",
        summary: "one sample of every AI session (table, or JSON with --json)",
        help: snapshot::HELP,
        run: snapshot::run,
    },
    Command {
        name: "cockpit",
        summary: "what to do per agent session: context, burn, re-read (contexto.py)",
        help: cockpit::HELP,
        run: cockpit::run,
    },
    Command {
        name: "insights",
        summary: "findings and a short briefing, optionally worded by a local model",
        help: insights::HELP,
        run: insights::run,
    },
    Command {
        name: "watch",
        summary: "sample every second until interrupted (table, or NDJSON with --json)",
        help: watch::HELP,
        run: watch::run,
    },
    Command {
        name: "signal",
        summary: "stop, resume, terminate or kill a session or one process",
        help: signal::HELP,
        run: signal::run,
    },
    Command {
        name: "reclaim",
        summary: "idle sessions by memory, with the command to end each (read only)",
        help: reclaim::HELP,
        run: reclaim::run,
    },
];

/// The top-level `--help`, built from `COMMANDS`.
pub fn usage() -> String {
    let mut s = String::from("usage: agentcenter-cli <command> [options]\n\ncommands:\n");
    for c in COMMANDS {
        s.push_str(&format!("  {:<10} {}\n", c.name, c.summary));
    }
    s.push_str("\nrun `agentcenter-cli <command> --help` for the options of a command");
    s
}

/// Opcoes de linha de comando de `snapshot` e `watch`.
#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub json: bool,
    pub interval_ms: u64,
    pub proc_root: Option<String>,
}

/// Opcoes comuns a `snapshot` e `watch`. `Ok(None)`: pediu `--help`.
pub fn parse(args: &[String], cmd: &str, help: &str) -> Result<Option<Options>, String> {
    let mut o = Options {
        json: false,
        interval_ms: 1000,
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
            "--interval-ms" => {
                let v = value("--interval-ms")?;
                o.interval_ms = v
                    .parse()
                    .map_err(|_| format!("--interval-ms: `{v}` is not a number of milliseconds"))?;
            }
            "--proc-root" => o.proc_root = Some(value("--proc-root")?),
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("{cmd}: unknown option `{other}`\n\n{help}")),
        }
    }
    Ok(Some(o))
}

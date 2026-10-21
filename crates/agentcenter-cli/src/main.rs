//! `agentcenter-cli`: AgentCenter sem interface grafica.

mod cmd;
mod table;

use std::process::ExitCode;

const USAGE: &str = "\
usage: agentcenter-cli <command> [options]

commands:
  snapshot   one sample of every AI session (table, or JSON with --json)
  cockpit    what to do per agent session: context, burn, re-read (contexto.py)
  watch      sample every second until interrupted (table, or NDJSON with --json)
  signal     stop, resume, terminate or kill a session or one process
  reclaim    idle sessions by memory, with the command to end each (read only)

run `agentcenter-cli <command> --help` for the options of a command";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("snapshot") => cmd::snapshot::run(args.collect()),
        Some("cockpit") => cmd::cockpit::run(args.collect()),
        Some("watch") => cmd::watch::run(args.collect()),
        Some("signal") => cmd::signal::run(args.collect()),
        Some("reclaim") => cmd::reclaim::run(args.collect()),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("agentcenter-cli {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
        None => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("agentcenter-cli: {msg}");
            ExitCode::from(2)
        }
    }
}

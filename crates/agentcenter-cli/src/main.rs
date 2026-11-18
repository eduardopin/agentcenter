//! `agentcenter-cli`: AgentCenter sem interface grafica.

mod cmd;
mod docs;
mod table;

use std::process::ExitCode;

fn main() -> ExitCode {
    let usage = cmd::usage();
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        // Hidden: scripts/gen-docs.sh writes docs/CLI.md and the man page from it.
        Some("gen-docs") => docs::run(args.collect()),
        Some("-h" | "--help") => {
            println!("{usage}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("agentcenter-cli {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(name) => match cmd::COMMANDS.iter().find(|c| c.name == name) {
            Some(c) => (c.run)(args.collect()),
            None => Err(format!("unknown command `{name}`\n\n{usage}")),
        },
        None => Err(usage),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("agentcenter-cli: {msg}");
            ExitCode::from(2)
        }
    }
}

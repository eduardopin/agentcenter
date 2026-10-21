//! Goose (block/goose, `download_cli.sh`): binario Rust `goose` -> `comm` = "goose".
//!
//! A ferramenta de migracao de banco pressly/goose tambem se chama `goose`. Os
//! dois so se distinguem pelo argv (`goose session` x `goose postgres ... up`),
//! e a cmdline so e lida para interpretadores: a migracao vira sessao enquanto
//! roda (segundos). O desktop (`goosed`) fica de fora.

use super::{AdapterSpec, AgentRole, HostedRule};

pub static GOOSE: AdapterSpec = AdapterSpec {
    id: "goose",
    role: AgentRole::Agent,
    comms: &["goose"],
    exe_names: &[],
    hosted: HostedRule::NONE,
};

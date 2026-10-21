//! Codex CLI: binario `codex`. Variantes cujo `comm` comeca com `codex`
//! (ex.: `codex-x86_64-un`, truncado) sao conferidas pelo executavel.

use super::{AdapterSpec, AgentRole, HostedRule};

pub static CODEX: AdapterSpec = AdapterSpec {
    id: "codex",
    role: AgentRole::Agent,
    comms: &["codex"],
    exe_names: &["codex"],
    hosted: HostedRule::NONE,
};

//! opencode (`npm i -g opencode-ai`, medido na 1.19 nesta maquina): o `bin` e um
//! ELF do Bun, `bin/opencode.exe`, chamado pelo link `opencode` -> `comm` =
//! "opencode", `exe` = "opencode.exe", cmdline `opencode`. Chamado pelo caminho
//! do `.exe`, o `comm` vira "opencode.exe" e o executavel confere.

use super::{AdapterSpec, AgentRole, HostedRule};

pub static OPENCODE: AdapterSpec = AdapterSpec {
    id: "opencode",
    role: AgentRole::Agent,
    comms: &["opencode"],
    exe_names: &["opencode", "opencode.exe"],
    hosted: HostedRule::NONE,
};

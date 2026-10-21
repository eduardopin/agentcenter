//! Kiro CLI (`curl -fsSL https://cli.kiro.dev/install | bash`, codigo fechado):
//! o instalador poe dois binarios em `~/.local/bin`, `kiro-cli` e
//! `kiro-cli-chat` (`BINARY_NAME`/`CHAT_BINARY_NAME` no install.sh).
//!
//! So `kiro-cli-chat` e sessao: e o processo do chat, pai dos MCP servers.
//! `kiro-cli` e o lancador e tambem roda subcomandos que nao sao agente (login,
//! doctor); casar os dois daria duas sessoes para um chat, como o loader do copilot.

use super::{AdapterSpec, AgentRole, HostedRule};

pub static KIRO: AdapterSpec = AdapterSpec {
    id: "kiro",
    role: AgentRole::Agent,
    comms: &["kiro-cli-chat"],
    exe_names: &[],
    hosted: HostedRule::NONE,
};

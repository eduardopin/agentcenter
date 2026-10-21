//! Claude Code: binario nativo `claude` (versoes em `~/.local/share/claude/versions/`).

use super::{AdapterSpec, AgentRole, HostedRule};

pub static CLAUDE: AdapterSpec = AdapterSpec {
    id: "claude",
    role: AgentRole::Agent,
    comms: &["claude"],
    exe_names: &[],
    hosted: HostedRule::NONE,
};

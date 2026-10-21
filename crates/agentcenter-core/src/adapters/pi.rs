//! pi: roda em node, que troca o titulo do processo para `pi` (`comm` = "pi").

use super::{AdapterSpec, AgentRole, HostedRule};

pub static PI: AdapterSpec = AdapterSpec {
    id: "pi",
    role: AgentRole::Agent,
    comms: &["pi"],
    exe_names: &[],
    hosted: HostedRule::NONE,
};

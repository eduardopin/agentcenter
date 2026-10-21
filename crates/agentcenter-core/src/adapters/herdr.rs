//! herdr: multiplexador. Servidor e clientes viram linhas proprias; agentes
//! dentro dos paineis sao mais profundos e ficam com as suas sessoes.

use super::{AdapterSpec, AgentRole, HostedRule};

pub static HERDR: AdapterSpec = AdapterSpec {
    id: "herdr",
    role: AgentRole::Multiplexer,
    comms: &["herdr"],
    exe_names: &[],
    hosted: HostedRule::NONE,
};

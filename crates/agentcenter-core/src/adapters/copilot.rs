//! GitHub Copilot CLI (`npm i -g @github/copilot`, medido na 1.0.98): o `bin`
//! `copilot` e o `npm-loader.js` (node), que faz `spawnSync` do binario nativo
//! do pacote `@github/copilot-linux-<arch>`, chamado `copilot` -> `comm` = "copilot".
//!
//! So o nativo e sessao. O loader (`node <prefixo>/bin/copilot`) fica acima dele
//! e nao pode casar: por isso nao ha regra de interpretador aqui. A 0.0.x, que
//! rodava o agente no proprio node, fica sem deteccao.

use super::{AdapterSpec, AgentRole, HostedRule};

pub static COPILOT: AdapterSpec = AdapterSpec {
    id: "copilot",
    role: AgentRole::Agent,
    comms: &["copilot"],
    exe_names: &[],
    hosted: HostedRule::NONE,
};

//! Gemini CLI (`npm i -g @google/gemini-cli`, medido na 0.64): node puro, sem
//! `process.title`, entao `comm` = "node" e o agente aparece no argv:
//! `node <prefixo>/bin/gemini` (shebang `#!/usr/bin/env node`, `bin` do
//! package.json = `bundle/gemini.js`). Layout antigo `@google/gemini-cli/dist/index.js`
//! segue o agtop (MIT, `src/matchers.rs` @ 7876b33).
//!
//! O gemini se relanca como filho node com `--max-old-space-size` (ver
//! `getNodeMemoryArgs` e `GEMINI_CLI_NO_RELAUNCH` no bundle): pai e filho casam.
//! Juntar os dois numa sessao so e da atribuicao (tree.rs), nao daqui.

use super::{AdapterSpec, AgentRole, HostedRule};

pub static GEMINI: AdapterSpec = AdapterSpec {
    id: "gemini",
    role: AgentRole::Agent,
    // Binario autonomo ou titulo trocado; o caso comum e o node abaixo.
    comms: &["gemini"],
    exe_names: &[],
    hosted: HostedRule {
        scripts: &["gemini"],
        modules: &[],
        paths: &[("/@google/gemini-cli/", "/index.js")],
    },
};

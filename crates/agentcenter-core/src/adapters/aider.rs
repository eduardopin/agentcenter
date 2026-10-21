//! Aider (`pip install aider-chat`, `uv tool`/`pipx`): entry point
//! `aider = aider.main:main`. Com shebang direto para o python do venv o kernel
//! poe o nome do script no `comm` ("aider"); com `#!/usr/bin/env python3` ou
//! `python -m aider` o `comm` e do interpretador e o agente esta no argv.
//! `python -m aider` segue o agtop (MIT, `src/matchers.rs` @ 7876b33).

use super::{AdapterSpec, AgentRole, HostedRule};

pub static AIDER: AdapterSpec = AdapterSpec {
    id: "aider",
    role: AgentRole::Agent,
    comms: &["aider"],
    exe_names: &[],
    hosted: HostedRule {
        scripts: &["aider"],
        modules: &["aider"],
        paths: &[],
    },
};

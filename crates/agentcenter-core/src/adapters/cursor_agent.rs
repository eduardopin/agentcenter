//! Cursor Agent CLI (`curl https://cursor.com/install -fsS | bash`, pacote
//! 2026.10.14): `~/.local/bin/{agent,cursor-agent}` apontam para
//! `~/.local/share/cursor-agent/versions/<v>/cursor-agent`, um bash que faz
//! `exec -a "$0" <dir>/node [--use-system-ca] <dir>/index.js`. Resultado:
//! `comm` = "node", argv0 = `agent` ou `cursor-agent`, script `index.js` sob
//! `/cursor-agent/versions/`. So o `index.js` casa: os workers do pacote
//! (`diff-worker.js`...) nao. `comm` "cursor-agent" segue o agtop
//! (MIT, `src/matchers.rs` @ 7876b33).

use super::{AdapterSpec, AgentRole, HostedRule};

pub static CURSOR_AGENT: AdapterSpec = AdapterSpec {
    id: "cursor-agent",
    role: AgentRole::Agent,
    comms: &["cursor-agent"],
    exe_names: &[],
    hosted: HostedRule {
        scripts: &[],
        modules: &[],
        paths: &[("/cursor-agent/versions/", "/index.js")],
    },
};

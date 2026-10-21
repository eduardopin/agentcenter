//! AGENTCENTER_EXTRA_AGENTS lido do ambiente. Arquivo proprio: unico teste que
//! mexe no ambiente deste binario de teste, sem corrida com outros.

use agentcenter_core::adapters::{EXTRA_AGENTS_ENV, Registry};
use agentcenter_core::{ProcRoot, Sampler};
use std::path::Path;

#[test]
fn agentcenter_extra_agents_env_adds_agents() {
    // SAFETY: este binario de teste tem um so teste; nenhuma outra thread le o ambiente.
    unsafe { std::env::set_var(EXTRA_AGENTS_ENV, "fakeagent") };
    let r = Registry::from_env();
    assert!(r.ids().contains(&"fakeagent"));
    let root =
        ProcRoot::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proc/nested"));
    let s = Sampler::default().tick(&root).unwrap();
    assert!(
        s.sessions
            .iter()
            .any(|x| x.agent == "fakeagent" && x.pid == 50)
    );
    unsafe { std::env::remove_var(EXTRA_AGENTS_ENV) };
    assert!(!Registry::from_env().ids().contains(&"fakeagent"));
}

//! Arvore de processos e atribuicao a sessoes (ARCHITECTURE, "Session model"):
//! cada processo pertence ao ancestral agente mais proximo, inclusive ele mesmo.

use std::collections::HashMap;

/// Resultado da atribuicao.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Attribution {
    /// pid -> pid do agente dono. Processo fora de qualquer sessao nao aparece.
    pub owner: HashMap<u32, u32>,
}

impl Attribution {
    /// `parents`: pid -> ppid de todos os processos lidos. `is_agent(pid)` diz se
    /// o processo e um agente. Ciclos (impossiveis no kernel, possiveis em
    /// fixture corrompida) sao cortados.
    pub fn build(parents: &HashMap<u32, u32>, mut is_agent: impl FnMut(u32) -> bool) -> Self {
        let agents: HashMap<u32, bool> = parents.keys().map(|&p| (p, is_agent(p))).collect();
        // Memo: pid -> Some(agente) | None (sem agente acima).
        let mut memo: HashMap<u32, Option<u32>> = HashMap::with_capacity(parents.len());
        let mut chain: Vec<u32> = Vec::new();
        for &start in parents.keys() {
            chain.clear();
            let mut cur = start;
            let found = loop {
                if let Some(&known) = memo.get(&cur) {
                    break known;
                }
                if agents.get(&cur).copied().unwrap_or(false) {
                    memo.insert(cur, Some(cur));
                    break Some(cur);
                }
                chain.push(cur);
                match parents.get(&cur) {
                    Some(&pp) if pp != 0 && pp != cur && chain.len() <= parents.len() => cur = pp,
                    _ => break None,
                }
            };
            for &p in &chain {
                memo.insert(p, found);
            }
        }
        let owner = memo
            .into_iter()
            .filter(|(pid, _)| parents.contains_key(pid))
            .filter_map(|(pid, a)| a.map(|a| (pid, a)))
            .collect();
        Attribution { owner }
    }

    /// pids dos agentes (donos de sessao).
    pub fn agents(&self) -> impl Iterator<Item = u32> + '_ {
        self.owner.iter().filter(|(p, a)| p == a).map(|(p, _)| *p)
    }
}

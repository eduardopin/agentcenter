//! Acoes sobre processos (ARCHITECTURE, "Actions"): suspender (SIGSTOP),
//! retomar (SIGCONT), terminar (SIGTERM) e matar (SIGKILL) uma sessao inteira
//! ou um processo.
//!
//! Em duas fases: [`plan`] le `/proc`, aplica as regras de seguranca e devolve
//! os alvos na ordem de envio, para o chamador mostrar e pedir confirmacao;
//! [`Plan::execute`] revalida cada alvo (starttime, campo 22 do `stat`)
//! imediatamente antes do sinal e pula pid reutilizado.
//!
//! Seguranca: so processos com Uid real igual ao nosso; nunca este processo,
//! seus ancestrais, o pid 1 ou o multiplexador (servidor herdr). O sinal sai
//! por `kill(2)` declarado aqui mesmo (a libc ja vem ligada pela std), sem
//! shell e sem dependencia nova.

use crate::adapters::{AgentRole, Registry};
use crate::procfs::smaps::parse_smaps_rollup;
use crate::procfs::stat::{Stat, parse_stat};
use crate::procfs::status::parse_status;
use crate::procfs::{ProcRoot, ReadError};
use crate::tree::Attribution;
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::io;
use std::path::Path;

mod sys {
    unsafe extern "C" {
        /// `kill(2)`. Inseguro de proposito: pid <= 0 atinge grupo ou todos os
        /// processos; o unico chamador ([`super::send_signal`]) barra isso.
        pub fn kill(pid: i32, sig: i32) -> i32;
        /// `getuid(2)`: sempre sucede, nao toca memoria.
        pub safe fn getuid() -> u32;
    }
}

// Numeros de sinal: SIGKILL e SIGTERM sao iguais em toda arquitetura; SIGSTOP e
// SIGCONT seguem a tabela generica (x86, arm, aarch64, riscv, powerpc, s390),
// exceto mips e sparc.
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
#[cfg(any(target_arch = "mips", target_arch = "mips64"))]
const SIGSTOP: i32 = 23;
#[cfg(any(target_arch = "mips", target_arch = "mips64"))]
const SIGCONT: i32 = 25;
#[cfg(any(target_arch = "sparc", target_arch = "sparc64"))]
const SIGSTOP: i32 = 17;
#[cfg(any(target_arch = "sparc", target_arch = "sparc64"))]
const SIGCONT: i32 = 19;
#[cfg(not(any(
    target_arch = "mips",
    target_arch = "mips64",
    target_arch = "sparc",
    target_arch = "sparc64"
)))]
const SIGSTOP: i32 = 19;
#[cfg(not(any(
    target_arch = "mips",
    target_arch = "mips64",
    target_arch = "sparc",
    target_arch = "sparc64"
)))]
const SIGCONT: i32 = 18;

/// Uid real deste processo.
pub fn current_uid() -> u32 {
    sys::getuid()
}

/// Envia `signal` ao processo `pid` por `kill(2)`. Recusa pid 0, 1 e acima de
/// `i32::MAX` (viraria negativo): esses valores atingem grupo, todos os
/// processos ou o init, nunca um alvo so.
pub fn send_signal(pid: u32, signal: i32) -> io::Result<()> {
    let target = i32::try_from(pid).ok().filter(|&p| p > 1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing kill({pid}): not a single process"),
        )
    })?;
    // SAFETY: kill(2) nao le nem escreve memoria nossa; `target` > 1 garante que
    // o sinal vai para um processo so.
    if unsafe { sys::kill(target, signal) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// O que fazer com os processos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// SIGSTOP: suspende.
    Stop,
    /// SIGCONT: retoma.
    Cont,
    /// SIGTERM: pede para terminar.
    Term,
    /// SIGKILL: mata.
    Kill,
}

impl Action {
    pub const ALL: [Action; 4] = [Action::Stop, Action::Cont, Action::Term, Action::Kill];

    /// `stop`, `cont`, `term` ou `kill`.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.verb() == s)
    }

    pub fn verb(self) -> &'static str {
        match self {
            Action::Stop => "stop",
            Action::Cont => "cont",
            Action::Term => "term",
            Action::Kill => "kill",
        }
    }

    pub fn signal(self) -> i32 {
        match self {
            Action::Stop => SIGSTOP,
            Action::Cont => SIGCONT,
            Action::Term => SIGTERM,
            Action::Kill => SIGKILL,
        }
    }

    pub fn signal_name(self) -> &'static str {
        match self {
            Action::Stop => "SIGSTOP",
            Action::Cont => "SIGCONT",
            Action::Term => "SIGTERM",
            Action::Kill => "SIGKILL",
        }
    }
}

/// Sobre o que agir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// O agente com este pid e a sua sessao (descendentes que nao sao de
    /// agente mais profundo, como no sampler).
    Session(u32),
    /// So este processo.
    Process(u32),
}

impl Target {
    pub fn pid(self) -> u32 {
        match self {
            Target::Session(p) | Target::Process(p) => p,
        }
    }
}

/// Por que um processo nao recebe o sinal. A mensagem diz o motivo ao usuario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// pid 1 (init).
    Init,
    /// O proprio processo que pede a acao.
    Myself { pid: u32 },
    /// Ancestral de quem pede: parar ou matar derruba quem pediu (o terminal,
    /// o agente que roda este comando).
    Ancestor { pid: u32, comm: String },
    /// Multiplexador (servidor herdr): segura as sessoes de todos os paineis.
    Multiplexer { pid: u32, agent: String },
    /// Processo de outro usuario.
    OtherUser { pid: u32, uid: u32, mine: u32 },
    /// Nao existe (ou ja saiu).
    NotFound { pid: u32 },
    /// `--session` num pid que nao e agente.
    NotASession { pid: u32, comm: String },
    /// `/proc` nao deixou ler o que a regra precisa.
    Unreadable { pid: u32, error: String },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Init => f.write_str("pid 1 (init) is never signalled"),
            Refusal::Myself { pid } => write!(f, "pid {pid} is this process"),
            Refusal::Ancestor { pid, comm } => write!(
                f,
                "pid {pid} ({comm}) is an ancestor of this process; signalling it would hit the caller"
            ),
            Refusal::Multiplexer { pid, agent } => write!(
                f,
                "pid {pid} is the {agent} server; it holds the sessions of every pane"
            ),
            Refusal::OtherUser { pid, uid, mine } => {
                write!(f, "pid {pid} belongs to uid {uid}, not to you (uid {mine})")
            }
            Refusal::NotFound { pid } => write!(f, "no process with pid {pid}"),
            Refusal::NotASession { pid, comm } => write!(
                f,
                "pid {pid} ({comm}) is not an agent; use --pid for a single process"
            ),
            Refusal::Unreadable { pid, error } => write!(f, "cannot read pid {pid}: {error}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Quem pede a acao: o uid e os pids que nunca sao alvo (ele mesmo e os
/// ancestrais).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guard {
    uid: u32,
    me: u32,
    ancestors: BTreeSet<u32>,
}

impl Guard {
    /// Este processo, lido de `root` (normalmente `/proc`).
    pub fn current(root: &ProcRoot) -> Self {
        Self::new(root, current_uid(), std::process::id())
    }

    /// `me` com o uid `uid`; os ancestrais sao lidos de `root` subindo pelo ppid.
    pub fn new(root: &ProcRoot, uid: u32, me: u32) -> Self {
        let mut ancestors = BTreeSet::new();
        let mut buf = String::new();
        let mut cur = me;
        while let Some(st) = read_stat(root, cur, &mut buf) {
            let pp = st.ppid;
            if pp == 0 || pp == cur || !ancestors.insert(pp) {
                break;
            }
            cur = pp;
        }
        Self { uid, me, ancestors }
    }

    pub fn uid(&self) -> u32 {
        self.uid
    }

    /// `Ok` se o processo pode receber sinal.
    fn check(&self, st: &Stat, uid: u32, role: Option<(&str, AgentRole)>) -> Result<(), Refusal> {
        let pid = st.pid;
        if pid <= 1 {
            return Err(Refusal::Init);
        }
        if pid == self.me {
            return Err(Refusal::Myself { pid });
        }
        if self.ancestors.contains(&pid) {
            return Err(Refusal::Ancestor {
                pid,
                comm: st.comm.clone(),
            });
        }
        if let Some((agent, AgentRole::Multiplexer)) = role {
            return Err(Refusal::Multiplexer {
                pid,
                agent: agent.to_string(),
            });
        }
        if uid != self.uid {
            return Err(Refusal::OtherUser {
                pid,
                uid,
                mine: self.uid,
            });
        }
        Ok(())
    }
}

/// Um processo que vai receber o sinal, como estava ao planejar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Victim {
    pub pid: u32,
    pub ppid: u32,
    pub comm: String,
    /// Campo 22 do `stat`: revalidado antes do sinal.
    pub starttime: u64,
    /// `Pss` e `SwapPss` do `smaps_rollup`; `None` se ilegivel.
    pub pss_bytes: Option<u64>,
    pub swap_pss_bytes: Option<u64>,
}

impl Victim {
    /// PSS + SwapPss, a memoria que sai com o processo. `None` se ilegivel.
    pub fn memory_bytes(&self) -> Option<u64> {
        Some(self.pss_bytes? + self.swap_pss_bytes.unwrap_or(0))
    }
}

/// Alvos de uma acao, na ordem de envio, e os processos da sessao que ficam
/// de fora (com o motivo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub action: Action,
    pub target: Target,
    /// Ordem de [`Plan::execute`]: raiz primeiro, depois por profundidade
    /// (pais antes de filhos). SIGCONT percorre de tras para frente.
    pub victims: Vec<Victim>,
    /// Processos da sessao que nao recebem o sinal (ex.: outro usuario).
    pub skipped: Vec<(u32, Refusal)>,
}

/// O que aconteceu com um alvo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    /// Saiu (ou virou zumbi) antes do sinal.
    Gone,
    /// O pid agora e de outro processo (starttime diferente): pulado.
    Reused,
    /// `kill(2)` ou a revalidacao falharam.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub pid: u32,
    pub delivery: Delivery,
}

fn read_stat(root: &ProcRoot, pid: u32, buf: &mut String) -> Option<Stat> {
    root.read_pid(pid, "stat", buf).ok()?;
    parse_stat(buf).ok()
}

fn exe_basename(root: &ProcRoot, pid: u32) -> Option<String> {
    let target = std::fs::read_link(root.pid_path(pid, "exe")).ok()?;
    let name = Path::new(&target).file_name()?.to_string_lossy();
    Some(name.trim_end_matches(" (deleted)").to_string())
}

/// Agente do processo pela mesma deteccao do sampler (`comm`, depois `exe`).
fn detect(registry: &Registry, root: &ProcRoot, st: &Stat) -> Option<(String, AgentRole)> {
    let mut exe = || exe_basename(root, st.pid);
    registry
        .detect(&st.comm, &mut exe)
        .map(|a| (a.id().to_string(), a.role()))
}

fn victim(root: &ProcRoot, st: &Stat, buf: &mut String) -> Victim {
    let rollup = root
        .read_pid(st.pid, "smaps_rollup", buf)
        .ok()
        .and_then(|()| parse_smaps_rollup(buf).ok());
    Victim {
        pid: st.pid,
        ppid: st.ppid,
        comm: st.comm.clone(),
        starttime: st.starttime,
        pss_bytes: rollup.map(|r| r.pss),
        swap_pss_bytes: rollup.map(|r| r.swap_pss),
    }
}

fn read_uid(root: &ProcRoot, pid: u32, buf: &mut String) -> Result<u32, Refusal> {
    let res = root
        .read_pid(pid, "status", buf)
        .and_then(|()| parse_status(buf));
    match res {
        Ok(s) => Ok(s.uid),
        Err(ReadError::Gone) => Err(Refusal::NotFound { pid }),
        Err(e) => Err(Refusal::Unreadable {
            pid,
            error: e.to_string(),
        }),
    }
}

/// Le `root` e monta o plano de `action` sobre `target`. `Err` se o alvo
/// principal (o processo, ou o agente da sessao) for recusado; processos da
/// sessao recusados vao para [`Plan::skipped`].
pub fn plan(
    root: &ProcRoot,
    registry: &Registry,
    guard: &Guard,
    action: Action,
    target: Target,
) -> Result<Plan, Refusal> {
    let pid = target.pid();
    if pid <= 1 {
        return Err(Refusal::Init);
    }
    let mut buf = String::new();
    let head = read_stat(root, pid, &mut buf).ok_or(Refusal::NotFound { pid })?;
    let head_role = detect(registry, root, &head);
    let head_uid = read_uid(root, pid, &mut buf)?;
    guard.check(
        &head,
        head_uid,
        head_role.as_ref().map(|(a, r)| (a.as_str(), *r)),
    )?;
    let mut plan = Plan {
        action,
        target,
        victims: vec![victim(root, &head, &mut buf)],
        skipped: Vec::new(),
    };
    if let Target::Process(_) = target {
        return Ok(plan);
    }
    if head_role.is_none() {
        return Err(Refusal::NotASession {
            pid,
            comm: head.comm,
        });
    }

    // Sessao: a mesma atribuicao do sampler sobre uma leitura completa.
    let pids = root.pids().map_err(|e| Refusal::Unreadable {
        pid,
        error: e.to_string(),
    })?;
    let mut stats: HashMap<u32, Stat> = HashMap::with_capacity(pids.len());
    for p in pids {
        if let Some(st) = read_stat(root, p, &mut buf) {
            stats.insert(p, st);
        }
    }
    // A raiz ja foi lida e validada; a leitura de agora vale para a arvore.
    stats.insert(pid, head);
    let parents: HashMap<u32, u32> = stats.iter().map(|(&p, s)| (p, s.ppid)).collect();
    let mut roles: HashMap<u32, (String, AgentRole)> = HashMap::new();
    let attribution = Attribution::build(&parents, |p| match detect(registry, root, &stats[&p]) {
        Some(r) => {
            roles.insert(p, r);
            true
        }
        None => false,
    });
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&p, &owner) in &attribution.owner {
        if owner == pid && p != pid {
            children.entry(stats[&p].ppid).or_default().push(p);
        }
    }
    for kids in children.values_mut() {
        kids.sort_unstable();
    }
    // Largura a partir da raiz: pai sempre antes do filho.
    let mut frontier = vec![pid];
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for parent in frontier {
            for &child in children.get(&parent).into_iter().flatten() {
                next.push(child);
                let st = &stats[&child];
                let role = roles.get(&child).map(|(a, r)| (a.as_str(), *r));
                let checked =
                    read_uid(root, child, &mut buf).and_then(|uid| guard.check(st, uid, role));
                match checked {
                    Ok(()) => plan.victims.push(victim(root, st, &mut buf)),
                    // Saiu entre a listagem e agora: nada a fazer com ele.
                    Err(Refusal::NotFound { .. }) => {}
                    Err(why) => plan.skipped.push((child, why)),
                }
            }
        }
        frontier = next;
    }
    Ok(plan)
}

impl Plan {
    /// Envia o sinal por `kill(2)`, na ordem da acao. Ver [`Plan::execute_with`].
    pub fn execute(&self, root: &ProcRoot) -> Vec<Outcome> {
        self.execute_with(root, send_signal)
    }

    /// Como [`Plan::execute`], com `send(pid, sinal)` no lugar de `kill(2)`.
    /// SIGSTOP, SIGTERM e SIGKILL vao da raiz para as folhas (o pai parado nao
    /// recria o filho); SIGCONT das folhas para a raiz (o filho volta antes do
    /// pai que o espera). Cada alvo e relido de `root` logo antes do sinal:
    /// starttime diferente e pid reutilizado, e o alvo e pulado.
    pub fn execute_with(
        &self,
        root: &ProcRoot,
        mut send: impl FnMut(u32, i32) -> io::Result<()>,
    ) -> Vec<Outcome> {
        let signal = self.action.signal();
        let mut buf = String::new();
        let order: Box<dyn Iterator<Item = &Victim>> = match self.action {
            Action::Cont => Box::new(self.victims.iter().rev()),
            _ => Box::new(self.victims.iter()),
        };
        order
            .map(|v| {
                let delivery = match read_stat(root, v.pid, &mut buf) {
                    None => Delivery::Gone,
                    Some(st) if st.starttime != v.starttime => Delivery::Reused,
                    Some(st) if st.state == 'Z' || st.state == 'X' => Delivery::Gone,
                    Some(_) => match send(v.pid, signal) {
                        Ok(()) => Delivery::Sent,
                        // ESRCH: saiu entre a releitura e o sinal.
                        Err(e) if e.raw_os_error() == Some(3) => Delivery::Gone,
                        Err(e) => Delivery::Failed(e.to_string()),
                    },
                };
                Outcome {
                    pid: v.pid,
                    delivery,
                }
            })
            .collect()
    }
}

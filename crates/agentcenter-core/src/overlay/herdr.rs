//! Overlay do herdr: workspace, aba, pane, titulo e status do agente de cada
//! sessao que roda num pane, lidos do socket API do herdr (protocolo 22).
//!
//! Processos: o herdr roda como um servidor (`herdr server`, dono dos panes e
//! do socket) e um cliente por terminal anexado (`herdr`, a TUI). So o servidor
//! escuta `herdr.sock` (`ss -xlp` mostra o pid dele); o cliente fala com o
//! servidor por `herdr-client.sock`. Os dois viram linhas `herdr` proprias no
//! sampler; este overlay so conversa com o socket.
//!
//! Socket, na ordem: `AGENTCENTER_HERDR_SOCKET`, `HERDR_SOCKET_PATH` (o herdr
//! poe no ambiente de cada pane; aponta para a sessao nomeada certa), depois
//! `$XDG_CONFIG_HOME/herdr/herdr.sock` e `$HOME/.config/herdr/herdr.sock` (a
//! sessao `default`; `herdr status server` e `herdr session list` mostram o
//! caminho). Variavel vazia conta como ausente.
//!
//! Casamento por pid: `agent.list` da os panes com agente e
//! `pane.process_info` da `shell_pid` e os pids dos processos em primeiro
//! plano de cada pane. A sessao casa com o pane que tem o pid do processo
//! agente dela e cujo tipo de agente (`claude`, `pi`, ...) e o mesmo da sessao;
//! nao depende do sessionId de outro overlay.
//!
//! Custo: o socket responde em 1 a 100 ms, longe do orcamento do tick. Uma
//! thread propria faz as requisicoes e publica um indice; [`Overlay::enrich`]
//! so consulta o indice (um lock e um lookup). A thread assina
//! `pane.agent_status_changed` (status aplicado na hora) e eventos de pane;
//! refaz `agent.list` no evento e no maximo a cada [`Timing::period`], e
//! `pane.process_info` so de pane novo ou cujo processo lider mudou. Nenhum
//! processo externo: o `herdr agent list` nao tem pid e falaria com o mesmo
//! socket. Socket ausente ou servidor parado: indice vazio, nada registrado
//! em log, nova tentativa a cada periodo.
//!
//! Ancora: a thread so sobe depois que o sampler entrega uma sessao `herdr`
//! que existe em `proc` (a raiz deste overlay, o `/proc` real em
//! [`Herdr::from_env`]) com o mesmo comm e o mesmo ppid. Sampler lendo um
//! `/proc` de fixture, ou que nao ve herdr nenhum, nao conecta ao socket: o pid
//! dos panes so tem sentido na maquina do servidor. Pid recusado nao e relido.
//!
//! Privacidade: `pane.process_info` traz argv, cmdline e cwd dos processos do
//! pane. Deles so o pid e lido; o resto nunca vira campo.

pub mod client;

use crate::adapters::herdr::HERDR;
use crate::overlay::{HerdrOverlay, Overlay, SessionOverlay};
use crate::procfs::stat::parse_stat;
use crate::procfs::{ProcRoot, read_file_into};
use crate::session::Session;
use client::{Event, Subscription};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::os::unix::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once, Weak};
use std::time::{Duration, Instant};

/// Socket explicito; vence `HERDR_SOCKET_PATH` e os caminhos padrao.
pub const SOCKET_ENV: &str = "AGENTCENTER_HERDR_SOCKET";

/// Eventos que mudam pane, agente ou ids e pedem um `agent.list` novo.
const TOPOLOGY_EVENTS: &[&str] = &[
    "pane.created",
    "pane.closed",
    "pane.moved",
    "pane.exited",
    "pane.agent_detected",
    "tab.moved",
    "workspace.moved",
    "workspace.closed",
];

/// Caminho do socket a partir de um leitor de ambiente (ver o topo do modulo).
pub fn socket_path(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    get(SOCKET_ENV)
        .or_else(|| get("HERDR_SOCKET_PATH"))
        .or_else(|| get("XDG_CONFIG_HOME").map(|d| d.join("herdr/herdr.sock")))
        .or_else(|| get("HOME").map(|h| h.join(".config/herdr/herdr.sock")))
}

/// Tipo de agente do herdr para o id de adapter do agentcenter. Tipo sem
/// equivalente fica como esta (casa com agente extra de mesmo `comm`).
pub fn adapter_id(kind: &str) -> &str {
    match kind {
        "cursor" => "cursor-agent",
        "open_code" => "opencode",
        "github_copilot" => "copilot",
        k => k,
    }
}

/// Um pane com agente, de `agent.list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPane {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub terminal_id: Option<String>,
    /// Tipo do agente segundo o herdr (`claude`, `pi`, `open_code`, ...).
    pub agent: Option<String>,
    /// `agent_status`: `idle`, `working`, `blocked`, `done`, `unknown`.
    pub status: Option<String>,
    /// `terminal_title_stripped`: titulo do terminal sem o icone de status.
    pub title: Option<String>,
}

fn text(o: &Value, k: &str) -> Option<String> {
    o.get(k).and_then(Value::as_str).map(str::to_string)
}

/// `result.agents` de uma resposta de `agent.list`. `None` se a linha nao e
/// resposta de sucesso com a lista. Entrada sem `pane_id`, `workspace_id` ou
/// `tab_id` em texto e pulada; campos desconhecidos sao ignorados.
pub fn parse_agent_list(line: &[u8]) -> Option<Vec<AgentPane>> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let agents = v.get("result")?.get("agents")?.as_array()?;
    Some(
        agents
            .iter()
            .filter_map(|a| {
                Some(AgentPane {
                    pane_id: text(a, "pane_id")?,
                    workspace_id: text(a, "workspace_id")?,
                    tab_id: text(a, "tab_id")?,
                    terminal_id: text(a, "terminal_id"),
                    agent: text(a, "agent"),
                    status: text(a, "agent_status"),
                    title: text(a, "terminal_title_stripped"),
                })
            })
            .collect(),
    )
}

/// Os pids de um pane, de `pane.process_info`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneProcesses {
    pub pane_id: String,
    pub shell_pid: Option<u32>,
    /// `foreground_process_group_id`.
    pub leader: Option<u32>,
    /// pids de `foreground_processes`, na ordem da resposta.
    pub pids: Vec<u32>,
}

fn pid(v: Option<&Value>) -> Option<u32> {
    v?.as_u64()?.try_into().ok()
}

/// `result.process_info` de uma resposta de `pane.process_info`. `None` se a
/// linha nao e resposta de sucesso (pane fechado: `pane_not_found`). So os
/// pids sao lidos: argv, cmdline e cwd ficam de fora.
pub fn parse_process_info(line: &[u8]) -> Option<PaneProcesses> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let info = v.get("result")?.get("process_info")?;
    Some(PaneProcesses {
        pane_id: text(info, "pane_id")?,
        shell_pid: pid(info.get("shell_pid")),
        leader: pid(info.get("foreground_process_group_id")),
        pids: info
            .get("foreground_processes")
            .and_then(Value::as_array)
            .map(|ps| ps.iter().filter_map(|p| pid(p.get("pid"))).collect())
            .unwrap_or_default(),
    })
}

/// Panes com agente e o pane de cada pid.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaneIndex {
    panes: HashMap<String, AgentPane>,
    by_pid: HashMap<u32, String>,
}

impl PaneIndex {
    /// Junta `agent.list` com os `pane.process_info`. Processos de pane que
    /// nao esta na lista sao ignorados; um pid em dois panes fica no primeiro.
    pub fn build(agents: Vec<AgentPane>, procs: &[PaneProcesses]) -> Self {
        let panes: HashMap<String, AgentPane> =
            agents.into_iter().map(|a| (a.pane_id.clone(), a)).collect();
        let mut by_pid = HashMap::new();
        for p in procs.iter().filter(|p| panes.contains_key(&p.pane_id)) {
            for pid in p.shell_pid.iter().chain(&p.pids) {
                by_pid.entry(*pid).or_insert_with(|| p.pane_id.clone());
            }
        }
        Self { panes, by_pid }
    }

    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }

    /// Panes com agente conhecidos, em ordem.
    pub fn pane_ids(&self) -> BTreeSet<String> {
        self.panes.keys().cloned().collect()
    }

    /// Campos do pane da sessao `agent` cujo processo agente e `pid`. Pane cujo
    /// tipo de agente e outro nao casa: um `codex` aberto pelo `claude` de um
    /// pane aparece entre os processos do pane, mas o pane e do `claude`.
    pub fn lookup(&self, agent: &str, pid: u32) -> Option<HerdrOverlay> {
        let pane = self.panes.get(self.by_pid.get(&pid)?)?;
        if pane
            .agent
            .as_deref()
            .is_some_and(|k| adapter_id(k) != agent)
        {
            return None;
        }
        Some(HerdrOverlay {
            workspace_id: Some(pane.workspace_id.clone()),
            tab_id: Some(pane.tab_id.clone()),
            pane_id: Some(pane.pane_id.clone()),
            title: pane.title.clone(),
            status: pane.status.clone(),
        })
    }

    /// Aplica um `pane.agent_status_changed`. `false` se o pane nao e conhecido.
    pub fn set_status(&mut self, pane_id: &str, status: String) -> bool {
        match self.panes.get_mut(pane_id) {
            Some(p) => {
                p.status = Some(status);
                true
            }
            None => false,
        }
    }
}

/// Intervalos da thread do overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// `agent.list` no maximo a cada `period` sem evento; tambem o intervalo
    /// entre tentativas com o socket ausente.
    pub period: Duration,
    /// Depois de um evento de topologia, espera ao menos isto desde o ultimo
    /// `agent.list` (rajada de eventos vira uma leitura so).
    pub debounce: Duration,
    /// Espera maxima por evento; tambem quanto a thread demora para notar que
    /// o overlay foi descartado.
    pub wait: Duration,
    /// Limite de cada leitura e escrita de uma requisicao.
    pub io_timeout: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            period: Duration::from_secs(5),
            debounce: Duration::from_secs(1),
            wait: Duration::from_secs(1),
            io_timeout: Duration::from_secs(2),
        }
    }
}

/// O que a thread publica e o overlay le.
#[derive(Default)]
struct Shared {
    index: Mutex<PaneIndex>,
    /// Sobe a cada mudanca publicada no indice.
    generation: AtomicU64,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut PaneIndex) -> bool) {
        let changed = f(&mut self.index.lock().unwrap_or_else(|p| p.into_inner()));
        if changed {
            self.generation.fetch_add(1, Ordering::Release);
        }
    }

    fn publish(&self, ix: PaneIndex) {
        self.update(|cur| {
            let changed = *cur != ix;
            *cur = ix;
            changed
        });
    }
}

/// Overlay do herdr. Preenche `overlay.herdr` das sessoes que rodam num pane.
pub struct Herdr {
    endpoint: Option<SocketAddr>,
    proc: ProcRoot,
    timing: Timing,
    shared: Arc<Shared>,
    start: Once,
    /// pids `herdr` que nao existem em `proc` (ver a ancora no topo).
    rejected: Mutex<Vec<u32>>,
}

impl Herdr {
    /// `endpoint`: socket do servidor (`None` desliga o overlay). `proc`: raiz
    /// onde conferir se o processo lider de um pane ainda e o mesmo.
    pub fn new(endpoint: Option<SocketAddr>, proc: ProcRoot) -> Self {
        Self::with_timing(endpoint, proc, Timing::default())
    }

    pub fn with_timing(endpoint: Option<SocketAddr>, proc: ProcRoot, timing: Timing) -> Self {
        Self {
            endpoint,
            proc,
            timing,
            shared: Arc::default(),
            start: Once::new(),
            rejected: Mutex::default(),
        }
    }

    /// Socket pelo ambiente (ver [`socket_path`]) e `/proc` real.
    pub fn from_env() -> Self {
        let endpoint =
            socket_path(|k| std::env::var_os(k)).and_then(|p| SocketAddr::from_pathname(p).ok());
        Self::new(endpoint, ProcRoot::default())
    }

    pub fn endpoint(&self) -> Option<&SocketAddr> {
        self.endpoint.as_ref()
    }

    /// Mudancas publicadas no indice ate agora (diagnostico e testes).
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// Copia do indice atual.
    pub fn index(&self) -> PaneIndex {
        self.shared
            .index
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Sobe a thread que le o socket, uma vez. [`Overlay::enrich`] chama isto
    /// sozinho ao ver o herdr ancorado; sem endpoint nao faz nada. A thread termina quando o overlay e
    /// descartado (em ate [`Timing::wait`] mais uma requisicao).
    pub fn start(&self) {
        let Some(addr) = self.endpoint.clone() else {
            return;
        };
        self.start.call_once(|| {
            let weak = Arc::downgrade(&self.shared);
            let proc = self.proc.clone();
            let timing = self.timing;
            // Sem thread nao ha overlay; o tick segue igual.
            let _ = std::thread::Builder::new()
                .name("herdr-overlay".into())
                .spawn(move || Worker::new(addr, proc, timing).run(&weak));
        });
    }
}

impl Herdr {
    /// A sessao `herdr` existe em `proc` com o mesmo comm e o mesmo ppid.
    fn anchored(&self, session: &Session) -> bool {
        let mut rejected = self.rejected.lock().unwrap_or_else(|p| p.into_inner());
        if rejected.contains(&session.pid) {
            return false;
        }
        let ppid = session
            .processes
            .iter()
            .find(|p| p.pid == session.pid)
            .map(|p| p.ppid);
        let mut text = String::new();
        let live = read_file_into(&self.proc.pid_path(session.pid, "stat"), &mut text).is_ok()
            && parse_stat(&text).is_ok_and(|st| {
                st.pid == session.pid
                    && HERDR.comms.contains(&st.comm.as_str())
                    && Some(st.ppid) == ppid
            });
        if !live {
            rejected.push(session.pid);
        }
        live
    }
}

impl Overlay for Herdr {
    fn id(&self) -> &str {
        "herdr"
    }

    fn enrich(&self, session: &Session, overlay: &mut SessionOverlay) {
        if self.endpoint.is_none() {
            return;
        }
        if session.agent == HERDR.id {
            if !self.start.is_completed() && self.anchored(session) {
                self.start();
            }
            return;
        }
        if !self.start.is_completed() {
            return;
        }
        let ix = self.shared.index.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(h) = ix.lookup(&session.agent, session.pid) {
            overlay.herdr = Some(h);
        }
    }
}

/// `pane.process_info` guardado enquanto o pane tem o mesmo terminal, o mesmo
/// agente e o mesmo processo lider (pid e starttime).
struct CachedPane {
    terminal_id: Option<String>,
    agent: Option<String>,
    leader: Option<(u32, u64)>,
    procs: PaneProcesses,
}

/// A thread do overlay: requisicoes, cache de `pane.process_info`, assinatura.
struct Worker {
    addr: SocketAddr,
    proc: ProcRoot,
    timing: Timing,
    cache: HashMap<String, CachedPane>,
    line: Vec<u8>,
    text: String,
    seq: u64,
}

impl Worker {
    fn new(addr: SocketAddr, proc: ProcRoot, timing: Timing) -> Self {
        Self {
            addr,
            proc,
            timing,
            cache: HashMap::new(),
            line: Vec::new(),
            text: String::new(),
            seq: 0,
        }
    }

    fn next_id(&mut self) -> String {
        self.seq += 1;
        format!("agentcenter:{}", self.seq)
    }

    /// Socket com caminho que nao existe: herdr ausente, sem tentar conectar.
    fn reachable(&self) -> bool {
        self.addr.as_pathname().is_none_or(Path::exists)
    }

    /// pid e starttime do processo lider do pane, se ele existe.
    fn leader(&mut self, p: &PaneProcesses) -> Option<(u32, u64)> {
        let pid = p.leader.or(p.shell_pid)?;
        read_file_into(&self.proc.pid_path(pid, "stat"), &mut self.text).ok()?;
        let st = parse_stat(&self.text).ok()?;
        (st.pid == pid).then_some((pid, st.starttime))
    }

    /// `agent.list` e o `pane.process_info` dos panes que mudaram.
    fn refresh(&mut self) -> Option<PaneIndex> {
        let id = self.next_id();
        let (addr, t) = (&self.addr, self.timing.io_timeout);
        client::request(
            addr,
            &id,
            "agent.list",
            Value::Object(Default::default()),
            t,
            &mut self.line,
        )
        .ok()?;
        let agents = parse_agent_list(&self.line)?;
        let mut procs = Vec::with_capacity(agents.len());
        for a in &agents {
            if let Some(c) = self.cache.get(&a.pane_id)
                && c.terminal_id == a.terminal_id
                && c.agent == a.agent
                && c.leader.is_some()
            {
                let (cached, procs_c) = (c.leader, c.procs.clone());
                if self.leader(&procs_c) == cached {
                    procs.push(procs_c);
                    continue;
                }
            }
            let id = self.next_id();
            let params = serde_json::json!({ "pane_id": a.pane_id });
            let fetched = client::request(
                &self.addr,
                &id,
                "pane.process_info",
                params,
                t,
                &mut self.line,
            )
            .ok()
            .and_then(|()| parse_process_info(&self.line))
            .filter(|p| p.pane_id == a.pane_id);
            let Some(p) = fetched else {
                self.cache.remove(&a.pane_id);
                continue;
            };
            let leader = self.leader(&p);
            self.cache.insert(
                a.pane_id.clone(),
                CachedPane {
                    terminal_id: a.terminal_id.clone(),
                    agent: a.agent.clone(),
                    leader,
                    procs: p.clone(),
                },
            );
            procs.push(p);
        }
        self.cache
            .retain(|pane, _| agents.iter().any(|a| &a.pane_id == pane));
        Some(PaneIndex::build(agents, &procs))
    }

    fn subscribe(&mut self, panes: &BTreeSet<String>) -> Option<Subscription> {
        let mut subs: Vec<Value> = TOPOLOGY_EVENTS
            .iter()
            .map(|t| serde_json::json!({ "type": t }))
            .collect();
        subs.extend(
            panes
                .iter()
                .map(|p| serde_json::json!({ "type": "pane.agent_status_changed", "pane_id": p })),
        );
        let id = self.next_id();
        Subscription::open(
            &self.addr,
            &id,
            subs,
            self.timing.io_timeout,
            self.timing.wait,
        )
        .ok()
    }

    fn run(mut self, shared: &Weak<Shared>) {
        let mut sub: Option<Subscription> = None;
        let mut subscribed = BTreeSet::new();
        let mut last: Option<Instant> = None;
        let mut dirty = false;
        loop {
            if shared.strong_count() == 0 {
                return;
            }
            let due = last.is_none_or(|l| {
                let e = l.elapsed();
                e >= self.timing.period || (dirty && e >= self.timing.debounce)
            });
            if due {
                last = Some(Instant::now());
                dirty = false;
                let ix = if self.reachable() {
                    self.refresh()
                } else {
                    None
                };
                let Some(sh) = shared.upgrade() else {
                    return;
                };
                match ix {
                    Some(ix) => {
                        let panes = ix.pane_ids();
                        sh.publish(ix);
                        drop(sh);
                        if sub.is_none() || panes != subscribed {
                            sub = self.subscribe(&panes);
                            subscribed = panes;
                        }
                    }
                    None => {
                        sh.publish(PaneIndex::default());
                        self.cache.clear();
                        sub = None;
                        subscribed.clear();
                    }
                }
            }
            match sub.as_mut().map(Subscription::next_event) {
                Some(Ok(Some(Event::Status { pane_id, status }))) => {
                    if let Some(sh) = shared.upgrade() {
                        sh.update(|ix| ix.set_status(&pane_id, status));
                    }
                }
                Some(Ok(Some(Event::Changed))) => dirty = true,
                Some(Ok(Some(Event::Other) | None)) => {}
                Some(Err(_)) => {
                    sub = None;
                    dirty = true;
                }
                None => std::thread::sleep(self.timing.wait),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn herdr_socket_path_order() {
        let all = env(&[
            (SOCKET_ENV, "/x.sock"),
            ("HERDR_SOCKET_PATH", "/s.sock"),
            ("XDG_CONFIG_HOME", "/c"),
            ("HOME", "/h"),
        ]);
        assert_eq!(socket_path(all), Some(PathBuf::from("/x.sock")));
        let pane = env(&[
            (SOCKET_ENV, ""),
            ("HERDR_SOCKET_PATH", "/s.sock"),
            ("HOME", "/h"),
        ]);
        assert_eq!(socket_path(pane), Some(PathBuf::from("/s.sock")));
        let xdg = env(&[("XDG_CONFIG_HOME", "/c"), ("HOME", "/h")]);
        assert_eq!(socket_path(xdg), Some(PathBuf::from("/c/herdr/herdr.sock")));
        let home = env(&[("HOME", "/h")]);
        assert_eq!(
            socket_path(home),
            Some(PathBuf::from("/h/.config/herdr/herdr.sock"))
        );
        assert_eq!(socket_path(env(&[])), None);
    }

    #[test]
    fn herdr_kind_maps_to_adapter_id() {
        assert_eq!(adapter_id("claude"), "claude");
        assert_eq!(adapter_id("pi"), "pi");
        assert_eq!(adapter_id("cursor"), "cursor-agent");
        assert_eq!(adapter_id("open_code"), "opencode");
        assert_eq!(adapter_id("github_copilot"), "copilot");
        assert_eq!(adapter_id("droid"), "droid");
    }

    #[test]
    fn herdr_agent_list_skips_entries_without_ids() {
        let line = br#"{"id":"1","result":{"agents":[
            {"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1","agent":"claude"},
            {"pane_id":"w1:p2","workspace_id":"w1"},
            {"pane_id":7,"workspace_id":"w1","tab_id":"w1:t3"}
        ]}}"#;
        let agents = parse_agent_list(line).unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].pane_id, "w1:p1");
        assert_eq!(agents[0].status, None);
        let err = br#"{"id":"1","error":{"code":"x","message":"y"}}"#;
        assert_eq!(parse_agent_list(err), None);
        assert_eq!(parse_agent_list(b"{"), None);
    }

    #[test]
    fn herdr_process_info_reads_only_pids() {
        let line = br#"{"id":"1","result":{"process_info":{"pane_id":"w1:p1",
            "shell_pid":10,"foreground_process_group_id":11,
            "foreground_processes":[{"pid":11,"name":"claude"},{"pid":"12"},{"pid":13,"name":"x"}]}}}"#;
        let p = parse_process_info(line).unwrap();
        assert_eq!(p.shell_pid, Some(10));
        assert_eq!(p.leader, Some(11));
        assert_eq!(p.pids, vec![11, 13]);
        let gone = br#"{"id":"1","error":{"code":"pane_not_found","message":"pane not found"}}"#;
        assert_eq!(parse_process_info(gone), None);
    }

    fn pane(id: &str, agent: Option<&str>) -> AgentPane {
        AgentPane {
            pane_id: id.into(),
            workspace_id: "w1".into(),
            tab_id: format!("{id}-tab"),
            terminal_id: None,
            agent: agent.map(str::to_string),
            status: Some("idle".into()),
            title: Some("t".into()),
        }
    }

    fn procs(id: &str, shell: u32, pids: &[u32]) -> PaneProcesses {
        PaneProcesses {
            pane_id: id.into(),
            shell_pid: Some(shell),
            leader: pids.first().copied(),
            pids: pids.to_vec(),
        }
    }

    #[test]
    fn herdr_index_matches_pid_and_kind() {
        let ix = PaneIndex::build(
            vec![pane("w1:p1", Some("claude")), pane("w1:p2", None)],
            &[
                procs("w1:p1", 10, &[11, 12]),
                procs("w1:p2", 20, &[21]),
                procs("w9:p9", 90, &[91]),
            ],
        );
        assert_eq!(
            ix.lookup("claude", 11).unwrap().pane_id.as_deref(),
            Some("w1:p1")
        );
        assert_eq!(
            ix.lookup("claude", 10).unwrap().pane_id.as_deref(),
            Some("w1:p1")
        );
        // Outro tipo no pane do claude: nao casa.
        assert_eq!(ix.lookup("codex", 12), None);
        // Pane sem tipo conhecido aceita qualquer agente pelo pid.
        assert_eq!(
            ix.lookup("codex", 21).unwrap().pane_id.as_deref(),
            Some("w1:p2")
        );
        // pid de pane fora do agent.list e pid desconhecido.
        assert_eq!(ix.lookup("claude", 91), None);
        assert_eq!(ix.lookup("claude", 99), None);
    }

    #[test]
    fn herdr_index_status_update() {
        let mut ix = PaneIndex::build(
            vec![pane("w1:p1", Some("claude"))],
            &[procs("w1:p1", 10, &[11])],
        );
        assert!(ix.set_status("w1:p1", "working".into()));
        assert!(!ix.set_status("w1:p9", "working".into()));
        assert_eq!(
            ix.lookup("claude", 11).unwrap().status.as_deref(),
            Some("working")
        );
    }

    #[test]
    fn herdr_without_endpoint_never_fills_or_starts() {
        let h = Herdr::new(None, ProcRoot::default());
        h.start();
        assert_eq!(h.generation(), 0);
        assert!(h.index().is_empty());
    }
}

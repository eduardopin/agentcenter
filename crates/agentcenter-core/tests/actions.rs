//! Acoes stop/cont/term/kill: arvore real criada no teste (lida de `/proc`) e
//! recusas sobre `/proc` real e fixtures.

use agentcenter_core::actions::{
    Action, Delivery, Guard, Outcome, Refusal, Target, plan, send_signal,
};
use agentcenter_core::adapters::Registry;
use agentcenter_core::fixture::{FakeProc, FakeProcess, TempDir};
use agentcenter_core::procfs::stat::parse_stat;
use agentcenter_core::{Plan, ProcRoot};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SIGTERM: i32 = 15;

fn stat_of(pid: u32) -> Option<(char, u32, String, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let st = parse_stat(&text).ok()?;
    Some((st.state, st.ppid, st.comm, st.starttime))
}

fn state(pid: u32) -> Option<char> {
    stat_of(pid).map(|s| s.0)
}

/// Vivo: o mesmo processo (starttime) ainda existe e nao e zumbi.
fn alive(pid: u32, starttime: u64) -> bool {
    stat_of(pid).is_some_and(|(s, _, _, t)| t == starttime && s != 'Z' && s != 'X')
}

fn until(limit: Duration, mut ok: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if ok() {
            return true;
        }
        if start.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Filhos de `pid` com este `comm`, lidos de `/proc`.
fn children(pid: u32, comm: &str) -> Vec<u32> {
    let mut out: Vec<u32> = ProcRoot::default()
        .pids()
        .unwrap()
        .into_iter()
        .filter(|&p| stat_of(p).is_some_and(|(_, pp, c, _)| pp == pid && c == comm))
        .collect();
    out.sort_unstable();
    out
}

/// Filho sem stdio herdado: um orfao parado nao segura o pipe de quem roda o teste.
fn quiet(cmd: &mut Command) -> Child {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Mata e colhe o filho no `Drop`, inclusive quando o teste falha.
struct Reap(Child);

impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `sh -c 'sleep 300 & sleep 300 & wait'`. No `Drop`, SIGKILL em tudo que sobrou.
struct Tree {
    sh: Child,
    pids: Vec<u32>,
}

impl Tree {
    fn spawn() -> Self {
        let sh = quiet(Command::new("sh").args(["-c", "sleep 300 & sleep 300 & wait"]));
        let mut t = Tree {
            pids: vec![sh.id()],
            sh,
        };
        let root = t.root();
        // Espera os dois filhos ja como `sleep` (antes do exec o comm e `sh`).
        assert!(
            until(Duration::from_secs(2), || children(root, "sleep").len()
                == 2),
            "sleeps nao apareceram"
        );
        t.pids.extend(children(root, "sleep"));
        t
    }

    fn root(&self) -> u32 {
        self.sh.id()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        for &p in &self.pids {
            let _ = send_signal(p, 9);
        }
        let _ = self.sh.wait();
    }
}

fn sorted(mut v: Vec<u32>) -> Vec<u32> {
    v.sort_unstable();
    v
}

fn victims(p: &Plan) -> Vec<u32> {
    p.victims.iter().map(|v| v.pid).collect()
}

fn all_sent(out: &[Outcome]) -> bool {
    out.iter().all(|o| o.delivery == Delivery::Sent)
}

/// Criterio 1: stop deixa a arvore toda em T, cont devolve a S, term mata tudo em 2 s.
#[test]
fn actions_stop_cont_term_on_a_real_tree() {
    let mut tree = Tree::spawn();
    let root = ProcRoot::default();
    // `sh` vira agente so neste teste, para a arvore ser uma sessao.
    let reg = Registry::with_extra("sh");
    let guard = Guard::current(&root);
    let sh = tree.root();
    let starts: Vec<(u32, u64)> = tree
        .pids
        .iter()
        .map(|&p| (p, stat_of(p).unwrap().3))
        .collect();

    let stop = plan(&root, &reg, &guard, Action::Stop, Target::Session(sh)).unwrap();
    assert_eq!(stop.victims[0].pid, sh, "raiz primeiro");
    assert_eq!(sorted(victims(&stop)), sorted(tree.pids.clone()));
    assert!(stop.skipped.is_empty(), "{:?}", stop.skipped);
    assert!(all_sent(&stop.execute(&root)));
    assert!(
        until(Duration::from_secs(2), || tree
            .pids
            .iter()
            .all(|&p| state(p) == Some('T'))),
        "stop: estados {:?}",
        tree.pids.iter().map(|&p| state(p)).collect::<Vec<_>>()
    );

    let cont = plan(&root, &reg, &guard, Action::Cont, Target::Session(sh)).unwrap();
    assert!(all_sent(&cont.execute(&root)));
    assert!(
        until(Duration::from_secs(2), || tree
            .pids
            .iter()
            .all(|&p| state(p) == Some('S'))),
        "cont: estados {:?}",
        tree.pids.iter().map(|&p| state(p)).collect::<Vec<_>>()
    );

    let term = plan(&root, &reg, &guard, Action::Term, Target::Session(sh)).unwrap();
    assert!(all_sent(&term.execute(&root)));
    assert!(
        until(Duration::from_secs(2), || {
            // Colhe o sh (filho deste teste) para ele nao ficar zumbi.
            let _ = tree.sh.try_wait();
            starts.iter().all(|&(p, t)| !alive(p, t))
        }),
        "term: ainda vivos {:?}",
        starts
            .iter()
            .filter(|&&(p, t)| alive(p, t))
            .collect::<Vec<_>>()
    );
}

/// STOP/TERM/KILL da raiz para as folhas; CONT das folhas para a raiz.
#[test]
fn actions_signal_order_root_first_cont_leaves_first() {
    let tree = Tree::spawn();
    let root = ProcRoot::default();
    let reg = Registry::with_extra("sh");
    let guard = Guard::current(&root);
    let sh = tree.root();
    for action in Action::ALL {
        let p = plan(&root, &reg, &guard, action, Target::Session(sh)).unwrap();
        let mut sent = Vec::new();
        p.execute_with(&root, |pid, sig| {
            sent.push((pid, sig));
            Ok(())
        });
        assert_eq!(sent.len(), 3, "{action:?}");
        assert!(sent.iter().all(|&(_, s)| s == action.signal()));
        let pos = sent.iter().position(|&(p, _)| p == sh).unwrap();
        let want = if action == Action::Cont { 2 } else { 0 };
        assert_eq!(pos, want, "{action:?}: raiz na posicao {pos} de {sent:?}");
    }
    // Nada foi enviado de verdade.
    assert!(tree.pids.iter().all(|&p| state(p) == Some('S')));
}

#[test]
fn actions_refuse_pid_1_self_and_ancestor() {
    let root = ProcRoot::default();
    let reg = Registry::default();
    let guard = Guard::current(&root);
    for target in [Target::Process(1), Target::Session(1)] {
        let e = plan(&root, &reg, &guard, Action::Kill, target).unwrap_err();
        assert_eq!(e, Refusal::Init);
        assert!(e.to_string().contains("pid 1"));
    }
    let me = std::process::id();
    let e = plan(&root, &reg, &guard, Action::Stop, Target::Process(me)).unwrap_err();
    assert_eq!(e, Refusal::Myself { pid: me });
    let parent = stat_of(me).unwrap().1;
    let e = plan(&root, &reg, &guard, Action::Stop, Target::Process(parent)).unwrap_err();
    assert!(
        matches!(e, Refusal::Ancestor { pid, .. } if pid == parent),
        "{e:?}"
    );
    assert!(e.to_string().contains("ancestor"));
}

#[test]
fn actions_send_signal_never_targets_a_group_or_init() {
    // Sinal 0 so testa permissao: mesmo que a guarda falhasse, nada acontece.
    for pid in [0, 1, u32::MAX, i32::MAX as u32 + 1] {
        let e = send_signal(pid, 0).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "pid {pid}");
    }
    assert!(send_signal(std::process::id(), 0).is_ok());
}

#[test]
fn actions_parse_verbs() {
    for a in Action::ALL {
        assert_eq!(Action::parse(a.verb()), Some(a));
    }
    assert_eq!(Action::parse("STOP"), None);
    assert_eq!(Action::parse("hup"), None);
    assert_eq!(Action::Term.signal(), 15);
    assert_eq!(Action::Kill.signal(), 9);
}

/// `/proc` falso: agente 600 (uid 1000) com bash 601, sudo 602 (uid 0) sob o
/// bash, node 603; claude 604 aninhado sob o bash com filho 605; herdr 10;
/// sleep 500 de root.
fn fake() -> (TempDir, FakeProc) {
    let dir = TempDir::new("ac-actions").unwrap();
    let fp = FakeProc::create(dir.path().join("proc")).unwrap();
    let mut init = FakeProcess::new(1, 0, "systemd");
    init.uid = 0;
    fp.add(&init).unwrap();
    fp.add(&FakeProcess::new(10, 1, "herdr")).unwrap();
    let mut rootly = FakeProcess::new(500, 1, "sleep");
    rootly.uid = 0;
    fp.add(&rootly).unwrap();
    fp.add(&FakeProcess::new(600, 1, "fakeagent")).unwrap();
    fp.add(&FakeProcess::new(601, 600, "bash")).unwrap();
    let mut sudo = FakeProcess::new(602, 601, "sudo");
    sudo.uid = 0;
    fp.add(&sudo).unwrap();
    fp.add(&FakeProcess::new(603, 600, "node")).unwrap();
    fp.add(&FakeProcess::new(604, 601, "claude")).unwrap();
    fp.add(&FakeProcess::new(605, 604, "node")).unwrap();
    (dir, fp)
}

/// Quem pede nao esta na fixture: sem ancestrais.
const OUTSIDER: u32 = 4_000_000;

#[test]
fn actions_refuse_other_user_and_skip_them_inside_a_session() {
    let (_d, fp) = fake();
    let root = fp.proc_root();
    let reg = Registry::with_extra("fakeagent");
    let guard = Guard::new(&root, 1000, OUTSIDER);
    let e = plan(&root, &reg, &guard, Action::Term, Target::Process(500)).unwrap_err();
    assert_eq!(
        e,
        Refusal::OtherUser {
            pid: 500,
            uid: 0,
            mine: 1000
        }
    );
    assert!(e.to_string().contains("uid 0"));
    // Na sessao, o processo de outro usuario fica de fora e o resto recebe;
    // o claude aninhado (e o filho dele) e outra sessao.
    let p = plan(&root, &reg, &guard, Action::Stop, Target::Session(600)).unwrap();
    assert_eq!(victims(&p), vec![600, 601, 603]);
    assert_eq!(p.skipped.len(), 1);
    assert_eq!(p.skipped[0].0, 602);
    assert!(matches!(p.skipped[0].1, Refusal::OtherUser { uid: 0, .. }));
    assert_eq!(p.victims[0].pss_bytes, Some(1024 * 1024));
    // Visto como root, o sleep de root vira alvo valido.
    let as_root = Guard::new(&root, 0, OUTSIDER);
    assert!(plan(&root, &reg, &as_root, Action::Term, Target::Process(500)).is_ok());
}

#[test]
fn actions_refuse_multiplexer_non_agent_missing_and_ancestor_in_fixture() {
    let (_d, fp) = fake();
    let root = fp.proc_root();
    let reg = Registry::with_extra("fakeagent");
    let guard = Guard::new(&root, 1000, OUTSIDER);
    for target in [Target::Process(10), Target::Session(10)] {
        let e = plan(&root, &reg, &guard, Action::Stop, target).unwrap_err();
        assert!(
            matches!(&e, Refusal::Multiplexer { pid: 10, agent } if agent == "herdr"),
            "{e:?}"
        );
    }
    let e = plan(&root, &reg, &guard, Action::Stop, Target::Session(601)).unwrap_err();
    assert!(matches!(e, Refusal::NotASession { pid: 601, .. }), "{e:?}");
    assert!(plan(&root, &reg, &guard, Action::Stop, Target::Process(601)).is_ok());
    let e = plan(&root, &reg, &guard, Action::Stop, Target::Process(4242)).unwrap_err();
    assert_eq!(e, Refusal::NotFound { pid: 4242 });
    // Quem pede e o node 605: 604, 601 e 600 sao ancestrais.
    let inside = Guard::new(&root, 1000, 605);
    let e = plan(&root, &reg, &inside, Action::Kill, Target::Session(600)).unwrap_err();
    assert!(matches!(e, Refusal::Ancestor { pid: 600, .. }), "{e:?}");
    let e = plan(&root, &reg, &inside, Action::Kill, Target::Process(605)).unwrap_err();
    assert_eq!(e, Refusal::Myself { pid: 605 });
}

#[test]
fn actions_skip_reused_pid_in_fixture() {
    let (_d, fp) = fake();
    let root = fp.proc_root();
    let reg = Registry::with_extra("fakeagent");
    let guard = Guard::new(&root, 1000, OUTSIDER);
    let p = plan(&root, &reg, &guard, Action::Term, Target::Process(603)).unwrap();
    let mut sent = Vec::new();
    let mut record = |pid, sig| {
        sent.push((pid, sig));
        Ok(())
    };
    // Controle: o mesmo processo recebe.
    let out = p.execute_with(&root, &mut record);
    assert_eq!(out[0].delivery, Delivery::Sent);
    // O pid 603 agora e outro processo (starttime diferente): pulado.
    let mut reused = FakeProcess::new(603, 600, "node");
    reused.starttime = 2000;
    fp.add(&reused).unwrap();
    let out = p.execute_with(&root, &mut record);
    assert_eq!(
        out,
        vec![Outcome {
            pid: 603,
            delivery: Delivery::Reused
        }]
    );
    // Sumiu: nada a enviar.
    std::fs::remove_dir_all(fp.root().join("603")).unwrap();
    let out = p.execute_with(&root, &mut record);
    assert_eq!(out[0].delivery, Delivery::Gone);
    assert_eq!(sent, vec![(603, SIGTERM)], "so o controle foi enviado");
}

/// Pid reutilizado em `/proc` real: o plano guarda um starttime que nao e o do
/// processo vivo, e o sinal nao sai.
#[test]
fn actions_skip_reused_pid_on_real_proc() {
    let child = Reap(quiet(Command::new("sleep").arg("300")));
    let pid = child.0.id();
    let root = ProcRoot::default();
    let guard = Guard::current(&root);
    let reg = Registry::default();
    let mut p = plan(&root, &reg, &guard, Action::Stop, Target::Process(pid)).unwrap();
    p.victims[0].starttime += 1;
    let out = p.execute(&root);
    assert_eq!(out[0].delivery, Delivery::Reused);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(state(pid), Some('S'), "sinal enviado a pid reutilizado");
    // Controle: com o starttime certo o mesmo plano para o processo.
    p.victims[0].starttime -= 1;
    assert!(all_sent(&p.execute(&root)));
    assert!(until(Duration::from_secs(2), || state(pid) == Some('T')));
}

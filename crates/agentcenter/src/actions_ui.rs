//! Acoes com confirmacao (C12): suspender, retomar, terminar e matar, pela
//! linha da lista, pelo cabecalho do detalhe, por processo na tabela e pela
//! pagina Reclaim. Sempre em tres passos, e so o terceiro envia sinal:
//!
//! 1. [`actions::plan`] numa thread de fundo (le o procfs; a thread do GTK
//!    nunca le). Recusa do core (outro usuario, a propria app, pid 1, o
//!    servidor herdr) vira toast com o motivo, sem dialogo.
//! 2. `AdwAlertDialog` com pid, comm e memoria de cada alvo, e os que ficam
//!    de fora. Terminar e matar sao destrutivos: botao destrutivo e Enter no
//!    Cancelar.
//! 3. So [`Prompt::answer`] com [`Response::Confirm`] entrega o plano ao
//!    [`Plan::execute`]. O plano mora dentro do [`Prompt`], sem acessor: nao ha
//!    outro caminho ate o sinal (o teste `execute_runs_only_after_confirm`).
//!
//! Nunca ha acao automatica: o unico gatilho e um clique (ou a sonda de
//! [`PROBE_ENV`], que responde ao mesmo dialogo e so existe por variavel de
//! ambiente). No modo fixture os pids sao de outra maquina: nada e planejado.

// Native session controls are attached by Detail; C12 signal actions stay separate.
pub mod session_control;

use crate::format::{DASH, bytes, sanitize};
use adw::prelude::*;
use agentcenter_core::actions::{self, Action, Delivery, Guard, Outcome, Plan, Refusal, Target};
use agentcenter_core::{ProcRoot, Registry};
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// Sonda do roteiro do agente falso e da captura da recusa:
/// `<stop|cont|term|kill>:<session|pid>:<PID>:<confirm|cancel|hold>`. Abre o
/// mesmo dialogo do clique e, depois de [`PROBE_DELAY`], responde por ele
/// (`hold` deixa o dialogo aberto para a captura). Imprime `action_probe=...`.
pub const PROBE_ENV: &str = "AGENTCENTER_ACTION_PROBE";
const PROBE_DELAY: Duration = Duration::from_millis(600);

/// Nome da acao da janela que os menus chamam, com `(sbus)`: verbo do core,
/// sessao inteira?, pid e o nome mostrado no dialogo.
pub const WIN_ACTION: &str = "act";

pub fn label(a: Action) -> &'static str {
    match a {
        Action::Stop => "Suspend",
        Action::Cont => "Resume",
        Action::Term => "Terminate",
        Action::Kill => "Kill",
    }
}

fn lower(a: Action) -> &'static str {
    match a {
        Action::Stop => "suspend",
        Action::Cont => "resume",
        Action::Term => "terminate",
        Action::Kill => "kill",
    }
}

fn past(a: Action) -> &'static str {
    match a {
        Action::Stop => "Suspended",
        Action::Cont => "Resumed",
        Action::Term => "Terminated",
        Action::Kill => "Killed",
    }
}

/// Terminar e matar acabam a sessao: o que o agente nao salvou se perde.
pub fn destructive(a: Action) -> bool {
    matches!(a, Action::Term | Action::Kill)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Resposta do dialogo. Qualquer id que nao seja [`Response::CONFIRM`]
/// (Cancelar, Esc, fechar) e cancelar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    Confirm,
    Cancel,
}

impl Response {
    pub const CONFIRM: &'static str = "confirm";
    pub const CANCEL: &'static str = "cancel";

    pub fn from_id(id: &str) -> Self {
        if id == Self::CONFIRM {
            Self::Confirm
        } else {
            Self::Cancel
        }
    }
}

/// Uma linha da lista de alvos do dialogo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VictimLine {
    pub pid: String,
    pub comm: String,
    pub memory: String,
}

/// Todo o texto do dialogo, sem GTK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptText {
    pub heading: String,
    pub body: String,
    pub confirm_label: String,
    pub destructive: bool,
    /// Enter confirma (suspender, retomar) ou cancela (terminar, matar).
    pub default_confirm: bool,
    pub victims: Vec<VictimLine>,
    /// Processos da sessao que nao recebem o sinal, com o motivo do core.
    pub skipped: Vec<String>,
}

/// Um plano a espera da resposta. O plano nao sai daqui por outro caminho
/// que nao [`Prompt::answer`] com confirmacao.
#[derive(Debug)]
pub struct Prompt {
    pub text: PromptText,
    plan: Plan,
    subject: String,
}

impl Prompt {
    pub fn action(&self) -> Action {
        self.plan.action
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// `execute` recebe o plano so com [`Response::Confirm`]; cancelar
    /// descarta o plano e devolve `None`.
    pub fn answer<R>(self, r: Response, execute: impl FnOnce(Plan) -> R) -> Option<R> {
        match r {
            Response::Confirm => Some(execute(self.plan)),
            Response::Cancel => None,
        }
    }
}

/// Texto do toast de uma recusa do core: a acao e o motivo dele.
pub fn refusal_toast(a: Action, why: &Refusal) -> String {
    format!("Cannot {}: {}", lower(a), sanitize(&why.to_string()))
}

/// Monta o dialogo de um plano, ou o toast da recusa. `subject` e o nome que
/// a tela mostra (sessao ou processo).
pub fn ask(a: Action, subject: &str, planned: Result<Plan, Refusal>) -> Result<Prompt, String> {
    let plan = planned.map_err(|why| refusal_toast(a, &why))?;
    let subject = sanitize(subject).into_owned();
    let n = plan.victims.len();
    let procs = plural(n, "process", "processes");
    let freed: u64 = plan.victims.iter().filter_map(|v| v.memory_bytes()).sum();
    let frees = if freed > 0 {
        format!(" Frees about {} of memory and swap.", bytes(freed as f64))
    } else {
        String::new()
    };
    let sig = plan.action.signal_name();
    let body = match plan.action {
        Action::Stop => format!(
            "Sends {sig} to {procs}. They keep their memory and use no CPU until you resume them."
        ),
        Action::Cont => format!("Sends {sig} to {procs}. They continue where they stopped."),
        Action::Term => format!(
            "Sends {sig} to {procs} and asks them to exit. Work the agent has not saved is lost.{frees}"
        ),
        Action::Kill => format!(
            "Sends {sig} to {procs}. They end at once, with no chance to clean up or save.{frees}"
        ),
    };
    let victims = plan
        .victims
        .iter()
        .map(|v| VictimLine {
            pid: v.pid.to_string(),
            comm: sanitize(&v.comm).into_owned(),
            memory: v
                .memory_bytes()
                .map_or_else(|| DASH.to_string(), |b| bytes(b as f64)),
        })
        .collect();
    let skipped = plan
        .skipped
        .iter()
        .map(|(_, why)| sanitize(&why.to_string()).into_owned())
        .collect();
    let text = PromptText {
        heading: format!("{} {subject}?", label(plan.action)),
        body,
        confirm_label: label(plan.action).to_string(),
        destructive: destructive(plan.action),
        default_confirm: !destructive(plan.action),
        victims,
        skipped,
    };
    Ok(Prompt {
        text,
        plan,
        subject,
    })
}

/// Toast do resultado: quantos receberam, quantos ja tinham saido, falhas.
pub fn outcome_toast(a: Action, subject: &str, outcomes: &[Outcome]) -> String {
    let sent = outcomes
        .iter()
        .filter(|o| o.delivery == Delivery::Sent)
        .count();
    let gone = outcomes
        .iter()
        .filter(|o| matches!(o.delivery, Delivery::Gone | Delivery::Reused))
        .count();
    let failed: Vec<&str> = outcomes
        .iter()
        .filter_map(|o| match &o.delivery {
            Delivery::Failed(e) => Some(e.as_str()),
            _ => None,
        })
        .collect();
    let subject = sanitize(subject);
    let mut s = if sent == 0 && failed.is_empty() {
        format!("Nothing to {}: {subject} had already exited", lower(a))
    } else {
        format!(
            "{} {subject}: {}",
            past(a),
            plural(sent, "process", "processes")
        )
    };
    if gone > 0 && sent > 0 {
        s.push_str(&format!(", {gone} had already exited"));
    }
    if let Some(first) = failed.first() {
        s.push_str(&format!(
            "; {} of {} failed ({})",
            failed.len(),
            outcomes.len(),
            sanitize(first)
        ));
    }
    s
}

/// A sonda de [`PROBE_ENV`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeSpec {
    pub action: Action,
    pub target: Target,
    /// `None`: `hold`, o dialogo fica aberto.
    pub response: Option<Response>,
}

pub fn parse_probe(v: &str) -> Option<ProbeSpec> {
    let mut it = v.trim().split(':');
    let action = Action::parse(it.next()?)?;
    let kind = it.next()?;
    let pid: u32 = it.next()?.parse().ok()?;
    let target = match kind {
        "session" => Target::Session(pid),
        "pid" => Target::Process(pid),
        _ => return None,
    };
    let response = match it.next()? {
        "confirm" => Some(Response::Confirm),
        "cancel" => Some(Response::Cancel),
        "hold" => None,
        _ => return None,
    };
    it.next().is_none().then_some(ProbeSpec {
        action,
        target,
        response,
    })
}

/// Parametro de [`WIN_ACTION`].
pub fn action_target(a: Action, target: Target, subject: &str) -> glib::Variant {
    let session = matches!(target, Target::Session(_));
    (
        a.verb().to_string(),
        session,
        target.pid(),
        subject.to_string(),
    )
        .to_variant()
}

pub fn parse_action_target(v: &glib::Variant) -> Option<(Action, Target, String)> {
    let (verb, session, pid, subject) = v.get::<(String, bool, u32, String)>()?;
    let target = if session {
        Target::Session(pid)
    } else {
        Target::Process(pid)
    };
    Some((Action::parse(&verb)?, target, subject))
}

/// Menu das quatro acoes sobre `target`, em duas secoes: as reversiveis e as
/// que acabam o processo. Reticencias: todas pedem confirmacao.
pub fn menu(target: Target, subject: &str) -> gio::Menu {
    let menu = gio::Menu::new();
    for group in [[Action::Stop, Action::Cont], [Action::Term, Action::Kill]] {
        let section = gio::Menu::new();
        for a in group {
            let item = gio::MenuItem::new(Some(&format!("{}…", label(a))), None);
            item.set_action_and_target_value(
                Some(&format!("win.{WIN_ACTION}")),
                Some(&action_target(a, target, subject)),
            );
            section.append_item(&item);
        }
        menu.append_section(None, &section);
    }
    menu
}

/// Botao "mais" que monta o menu na hora de abrir, com o alvo do momento:
/// linhas recicladas trocam de sessao, entao nada e fixado no setup.
pub fn menu_button(
    a11y: &str,
    target: impl Fn() -> Option<(Target, String)> + 'static,
) -> gtk::MenuButton {
    let b = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(a11y)
        .valign(gtk::Align::Center)
        .build();
    b.add_css_class("flat");
    b.add_css_class("ac-actions");
    b.update_property(&[gtk::accessible::Property::Label(a11y)]);
    b.set_create_popup_func(move |b| {
        let model = target().map(|(t, s)| menu(t, &s));
        b.set_menu_model(model.as_ref());
    });
    b
}

fn plan_now(a: Action, target: Target) -> Result<Plan, Refusal> {
    let root = ProcRoot::default();
    let guard = Guard::current(&root);
    actions::plan(&root, &Registry::from_env(), &guard, a, target)
}

fn execute_now(plan: Plan) -> Vec<Outcome> {
    plan.execute(&ProcRoot::default())
}

fn caption(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.add_css_class("ac-dialog-caption");
    l
}

/// O `AdwAlertDialog` do plano.
pub fn dialog(t: &PromptText) -> adw::AlertDialog {
    let d = adw::AlertDialog::new(Some(&t.heading), Some(&t.body));
    let grid = gtk::Grid::builder()
        .column_spacing(16)
        .row_spacing(2)
        .build();
    grid.add_css_class("ac-victims");
    for (i, h) in ["PID", "Process", "PSS + swap"].iter().enumerate() {
        let l = caption(h);
        if i == 2 {
            l.set_xalign(1.0);
        }
        grid.attach(&l, i as i32, 0, 1, 1);
    }
    for (row, v) in t.victims.iter().enumerate() {
        let r = row as i32 + 1;
        let pid = gtk::Label::new(Some(&v.pid));
        pid.set_xalign(1.0);
        pid.add_css_class("monospace");
        let comm = gtk::Label::new(Some(&v.comm));
        comm.set_xalign(0.0);
        comm.set_hexpand(true);
        comm.set_ellipsize(gtk::pango::EllipsizeMode::End);
        comm.add_css_class("monospace");
        let mem = gtk::Label::new(Some(&v.memory));
        mem.set_xalign(1.0);
        mem.add_css_class("numeric");
        grid.attach(&pid, 0, r, 1, 1);
        grid.attach(&comm, 1, r, 1, 1);
        grid.attach(&mem, 2, r, 1, 1);
    }
    let scroll = gtk::ScrolledWindow::builder()
        .child(&grid)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(220)
        .build();
    let extra = gtk::Box::new(gtk::Orientation::Vertical, 8);
    extra.append(&scroll);
    if !t.skipped.is_empty() {
        extra.append(&caption("Not signalled"));
        for s in &t.skipped {
            let l = gtk::Label::new(Some(s));
            l.set_xalign(0.0);
            l.set_wrap(true);
            l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            l.add_css_class("ac-dialog-note");
            extra.append(&l);
        }
    }
    d.set_extra_child(Some(&extra));
    d.add_response(Response::CANCEL, "Cancel");
    d.add_response(Response::CONFIRM, &t.confirm_label);
    if t.destructive {
        d.set_response_appearance(Response::CONFIRM, adw::ResponseAppearance::Destructive);
    }
    d.set_default_response(Some(if t.default_confirm {
        Response::CONFIRM
    } else {
        Response::CANCEL
    }));
    d.set_close_response(Response::CANCEL);
    d
}

/// Dono do fluxo na janela: o pai dos dialogos e o overlay dos toasts.
pub struct Actions {
    parent: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    /// `false` no modo fixture: os pids nao sao desta maquina.
    enabled: bool,
    /// Um fluxo por vez: um segundo pedido com dialogo aberto e ignorado.
    busy: Cell<bool>,
    probe: Cell<Option<ProbeSpec>>,
    probing: bool,
}

impl Actions {
    pub fn new(
        parent: &adw::ApplicationWindow,
        toasts: &adw::ToastOverlay,
        enabled: bool,
    ) -> Rc<Self> {
        let probe = std::env::var(PROBE_ENV).ok().and_then(|v| parse_probe(&v));
        Rc::new(Self {
            parent: parent.clone(),
            toasts: toasts.clone(),
            enabled,
            busy: Cell::new(false),
            probe: Cell::new(probe),
            probing: probe.is_some(),
        })
    }

    /// Instala [`WIN_ACTION`] na janela.
    pub fn install(self: &Rc<Self>) {
        let act = gio::SimpleAction::new(WIN_ACTION, Some(glib::VariantTy::new("(sbus)").unwrap()));
        let this = Rc::downgrade(self);
        act.connect_activate(move |_, v| {
            let (Some(this), Some((a, t, s))) = (this.upgrade(), v.and_then(parse_action_target))
            else {
                return;
            };
            this.request(a, t, s);
        });
        self.parent.add_action(&act);
    }

    pub fn toast(&self, text: &str) {
        if self.probing {
            println!("toast={text}");
        }
        let t = adw::Toast::new(text);
        t.set_use_markup(false);
        t.set_timeout(6);
        self.toasts.add_toast(t);
    }

    /// Roda a sonda uma vez (no primeiro frame).
    pub fn run_probe(self: &Rc<Self>) {
        if let Some(p) = self.probe.get() {
            // Sem a entrada animada do toast: com a tela apagada o relogio de
            // frames nao anda e a captura sai sem o toast (como em probe_back).
            if std::env::var_os("AGENTCENTER_SCREENSHOT").is_some()
                && let Some(settings) = gtk::Settings::default()
            {
                settings.set_gtk_enable_animations(false);
            }
            self.request(p.action, p.target, format!("pid {}", p.target.pid()));
        }
    }

    /// Passo 1: planeja fora da thread do GTK.
    pub fn request(self: &Rc<Self>, a: Action, target: Target, subject: String) {
        if !self.enabled {
            self.toast("Actions are off: AgentCenter is showing a recorded snapshot");
            return;
        }
        if self.busy.replace(true) {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let planned = gio::spawn_blocking(move || plan_now(a, target)).await;
            let planned = match planned {
                Ok(p) => p,
                Err(_) => {
                    this.busy.set(false);
                    this.toast(&format!("Cannot {}: the planner stopped", lower(a)));
                    return;
                }
            };
            match ask(a, &subject, planned) {
                Err(msg) => {
                    if this.probing {
                        println!("action_probe=refused");
                    }
                    this.busy.set(false);
                    this.toast(&msg);
                }
                Ok(prompt) => this.present(prompt),
            }
        });
    }

    /// Passo 2: o dialogo. Passo 3 so na resposta de confirmacao.
    fn present(self: &Rc<Self>, prompt: Prompt) {
        let d = dialog(&prompt.text);
        if self.probing {
            let pids: Vec<String> = prompt
                .text
                .victims
                .iter()
                .map(|v| format!("{}:{}", v.pid, v.comm))
                .collect();
            println!(
                "action_probe=prompt heading={:?} victims={}",
                prompt.text.heading,
                pids.join(",")
            );
        }
        let slot = Rc::new(RefCell::new(Some(prompt)));
        let this = self.clone();
        d.connect_response(None, move |_, id| {
            // Esc ou fechar depois da resposta: o plano ja saiu do slot.
            let Some(prompt) = slot.borrow_mut().take() else {
                return;
            };
            let (a, subject) = (prompt.action(), prompt.subject().to_string());
            let t = this.clone();
            let ran = prompt.answer(Response::from_id(id), move |plan| {
                glib::spawn_future_local(async move {
                    let outcomes = gio::spawn_blocking(move || execute_now(plan)).await;
                    t.busy.set(false);
                    match outcomes {
                        Ok(o) => t.toast(&outcome_toast(a, &subject, &o)),
                        Err(_) => {
                            t.toast(&format!("Cannot {}: the signal thread stopped", lower(a)))
                        }
                    }
                });
            });
            if ran.is_none() {
                if this.probing {
                    println!("action_probe=cancelled");
                }
                this.busy.set(false);
            }
        });
        d.present(Some(&self.parent));
        if let Some(r) = self.probe.take().and_then(|p| p.response) {
            let d = d.clone();
            glib::timeout_add_local_once(PROBE_DELAY, move || {
                let id = match r {
                    Response::Confirm => Response::CONFIRM,
                    Response::Cancel => Response::CANCEL,
                };
                d.emit_by_name::<()>("response", &[&id]);
                d.force_close();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::actions::Victim;

    fn plan(a: Action) -> Plan {
        Plan {
            action: a,
            target: Target::Session(4242),
            victims: vec![
                Victim {
                    pid: 4242,
                    ppid: 1,
                    comm: "fake-agent".into(),
                    starttime: 7,
                    pss_bytes: Some(3 << 20),
                    swap_pss_bytes: Some(1 << 20),
                },
                Victim {
                    pid: 4243,
                    ppid: 4242,
                    comm: "node\x1b[2J".into(),
                    starttime: 8,
                    pss_bytes: None,
                    swap_pss_bytes: None,
                },
            ],
            skipped: vec![(
                4244,
                Refusal::OtherUser {
                    pid: 4244,
                    uid: 0,
                    mine: 1000,
                },
            )],
        }
    }

    #[test]
    fn execute_runs_only_after_confirm() {
        for a in Action::ALL {
            let calls = Cell::new(0);
            // Cancelar, Esc e fechar: o executor nunca roda.
            for id in [Response::CANCEL, "close", "", "Confirm"] {
                let p = ask(a, "kiln", Ok(plan(a))).unwrap();
                let r = p.answer(Response::from_id(id), |_| calls.set(calls.get() + 1));
                assert!(r.is_none(), "{a:?} {id:?}");
            }
            assert_eq!(calls.get(), 0, "{a:?}: executed without confirm");
            // Confirmar: roda uma vez, com o plano que o dialogo mostrou.
            let p = ask(a, "kiln", Ok(plan(a))).unwrap();
            let shown: Vec<String> = p.text.victims.iter().map(|v| v.pid.clone()).collect();
            let got = p.answer(Response::from_id(Response::CONFIRM), |plan| {
                calls.set(calls.get() + 1);
                plan.victims
                    .iter()
                    .map(|v| v.pid.to_string())
                    .collect::<Vec<_>>()
            });
            assert_eq!(calls.get(), 1);
            assert_eq!(got, Some(shown));
        }
    }

    #[test]
    fn refusal_never_reaches_a_dialog() {
        let r = ask(
            Action::Stop,
            "agentcenter",
            Err(Refusal::Myself { pid: 77 }),
        );
        assert_eq!(r.unwrap_err(), "Cannot suspend: pid 77 is this process");
        let r = ask(
            Action::Kill,
            "herdr",
            Err(Refusal::Multiplexer {
                pid: 5,
                agent: "herdr".into(),
            }),
        );
        assert_eq!(
            r.unwrap_err(),
            "Cannot kill: pid 5 is the herdr server; it holds the sessions of every pane"
        );
        assert_eq!(
            ask(Action::Term, "x", Err(Refusal::Init)).unwrap_err(),
            "Cannot terminate: pid 1 (init) is never signalled"
        );
    }

    #[test]
    fn dialog_text_lists_pid_comm_memory_and_the_skipped() {
        let t = ask(Action::Kill, "kiln-auth", Ok(plan(Action::Kill)))
            .unwrap()
            .text;
        assert_eq!(t.heading, "Kill kiln-auth?");
        assert!(
            t.body.starts_with("Sends SIGKILL to 2 processes."),
            "{}",
            t.body
        );
        assert!(t.body.contains("Frees about 4.0 MiB"), "{}", t.body);
        assert!(t.destructive && !t.default_confirm);
        assert_eq!(t.victims[0].pid, "4242");
        assert_eq!(t.victims[0].comm, "fake-agent");
        assert_eq!(t.victims[0].memory, "4.0 MiB");
        assert_eq!(t.victims[1].memory, DASH);
        assert!(
            !t.victims[1].comm.contains('\x1b'),
            "{:?}",
            t.victims[1].comm
        );
        assert_eq!(
            t.skipped,
            ["pid 4244 belongs to uid 0, not to you (uid 1000)"]
        );

        let t = ask(Action::Stop, "kiln", Ok(plan(Action::Stop)))
            .unwrap()
            .text;
        assert!(!t.destructive && t.default_confirm);
        assert_eq!(t.confirm_label, "Suspend");
        assert!(
            ask(Action::Term, "k", Ok(plan(Action::Term)))
                .unwrap()
                .text
                .destructive
        );
        assert!(
            !ask(Action::Cont, "k", Ok(plan(Action::Cont)))
                .unwrap()
                .text
                .destructive
        );
    }

    #[test]
    fn outcome_toast_counts_sent_gone_and_failed() {
        let o = |pid, delivery| Outcome { pid, delivery };
        assert_eq!(
            outcome_toast(
                Action::Stop,
                "kiln",
                &[o(1, Delivery::Sent), o(2, Delivery::Sent)]
            ),
            "Suspended kiln: 2 processes"
        );
        assert_eq!(
            outcome_toast(
                Action::Term,
                "kiln",
                &[o(1, Delivery::Sent), o(2, Delivery::Gone)]
            ),
            "Terminated kiln: 1 process, 1 had already exited"
        );
        assert_eq!(
            outcome_toast(Action::Kill, "kiln", &[o(1, Delivery::Gone)]),
            "Nothing to kill: kiln had already exited"
        );
        assert_eq!(
            outcome_toast(
                Action::Cont,
                "kiln",
                &[
                    o(1, Delivery::Sent),
                    o(2, Delivery::Failed("Operation not permitted".into()))
                ]
            ),
            "Resumed kiln: 1 process; 1 of 2 failed (Operation not permitted)"
        );
    }

    #[test]
    fn probe_spec_parses_only_the_documented_form() {
        assert_eq!(
            parse_probe("stop:session:42:confirm"),
            Some(ProbeSpec {
                action: Action::Stop,
                target: Target::Session(42),
                response: Some(Response::Confirm)
            })
        );
        assert_eq!(parse_probe("kill:pid:7:hold").unwrap().response, None);
        for bad in [
            "",
            "stop",
            "stop:session:42",
            "nuke:pid:1:confirm",
            "stop:pid:x:confirm",
            "stop:pid:1:yes",
            "stop:pid:1:confirm:x",
        ] {
            assert_eq!(parse_probe(bad), None, "{bad}");
        }
    }

    #[test]
    fn action_target_round_trips() {
        let v = action_target(Action::Term, Target::Process(99), "node (pid 99)");
        assert_eq!(
            parse_action_target(&v),
            Some((
                Action::Term,
                Target::Process(99),
                "node (pid 99)".to_string()
            ))
        );
        let v = action_target(Action::Stop, Target::Session(5), "kiln");
        assert_eq!(parse_action_target(&v).unwrap().1, Target::Session(5));
        assert_eq!(
            parse_action_target(&("bogus".to_string(), true, 1u32, String::new()).to_variant()),
            None
        );
    }
}

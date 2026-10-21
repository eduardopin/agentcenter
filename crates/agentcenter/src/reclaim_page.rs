//! Pagina Reclaim (C12): as sessoes ociosas, maior PSS + SwapPss primeiro, o
//! total que sairia com elas e um "Terminate…" por linha, que abre o mesmo
//! dialogo das outras acoes. Nada e terminado sem confirmacao, e nada aqui
//! dispara sozinho.
//!
//! A regra e a do core ([`agentcenter_core::reclaim`]), a mesma do
//! `agentcenter-cli reclaim`: a janela sao os ultimos
//! [`DEFAULT_WINDOW_S`] ticks com taxas, guardados como `Arc<Snapshot>`
//! (o mesmo objeto do feed, sem copia). O `CpuWindow` e refeito dessa janela
//! a cada tick, entao ela desliza; o do core so cresce. Ate a janela encher,
//! a pagina diz que esta observando, como o CLI que espera os 10 s.
//!
//! ```text
//! Reclaimable  3.1 GiB   in 4 idle sessions · watched 10 s
//! Idle: the agent says so, or ... Nothing ends until you confirm.
//! ─────────────────────────────────────────────────────────────
//! ledger-i18n                                   1.4 GiB  ▮▮▮▯▯
//! claude · ~/src/ledger · pid 3121              [Terminate…]
//! Idle by status
//! ```

use crate::actions_ui;
use crate::format::{DASH, Row, bytes, cpu_pct, sanitize};
use adw::prelude::*;
use agentcenter_core::actions::{Action, Target};
use agentcenter_core::reclaim::{self, DEFAULT_WINDOW_S, IDLE_MAX_CPU_PCT};
use agentcenter_core::{CpuWindow, IdleBy, Reclaim, Snapshot};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;

/// Kernel identity guards live navigation; overlay fallback is fixture-only at the caller.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionLink {
    pub pid: u32,
    pub overlay_id: Option<String>,
    pub starttime: Option<u64>,
}

impl SessionLink {
    pub fn matches(&self, snap: &Snapshot) -> bool {
        if let Some(start) = self.starttime {
            return snap.sessions.iter().any(|s| {
                s.pid == self.pid
                    && s.starttime == Some(start)
                    && self
                        .overlay_id
                        .as_deref()
                        .filter(|id| !id.is_empty())
                        .is_none_or(|id| {
                            s.overlay
                                .as_ref()
                                .and_then(|o| o.agent_session_id.as_deref())
                                == Some(id)
                        })
            });
        }
        let Some(id) = self.overlay_id.as_deref().filter(|id| !id.is_empty()) else {
            return false;
        };
        snap.sessions.iter().any(|s| {
            s.pid == self.pid
                && s.overlay
                    .as_ref()
                    .and_then(|o| o.agent_session_id.as_deref())
                    == Some(id)
        })
    }
}

type OpenSession = Rc<dyn Fn(&SessionLink, &gtk::Button)>;

/// Ticks da janela, os mesmos do `agentcenter-cli reclaim` padrao.
pub const WINDOW_TICKS: usize = DEFAULT_WINDOW_S as usize;

const STACK_WATCHING: &str = "watching";
const STACK_EMPTY: &str = "empty";
const STACK_LIST: &str = "list";

/// Janela deslizante dos ultimos [`WINDOW_TICKS`] snapshots com taxas.
#[derive(Default)]
pub struct Ring {
    snaps: VecDeque<Arc<Snapshot>>,
}

impl Ring {
    /// Guarda o tick; o primeiro do sampler (sem taxas) nao conta.
    pub fn push(&mut self, snap: &Arc<Snapshot>) {
        if snap.interval_ms == 0 {
            return;
        }
        if self.snaps.len() == WINDOW_TICKS {
            self.snaps.pop_front();
        }
        self.snaps.push_back(Arc::clone(snap));
    }

    pub fn len(&self) -> usize {
        self.snaps.len()
    }

    /// O reclaim da janela cheia, como o CLI: o ultimo tick e o `CpuWindow`
    /// dos [`WINDOW_TICKS`]. `None` ate encher.
    pub fn reclaim(&self, own_pid: Option<u32>) -> Option<Reclaim> {
        if self.snaps.len() < WINDOW_TICKS {
            return None;
        }
        let mut w = CpuWindow::new();
        for s in &self.snaps {
            w.observe(s);
        }
        let last = self.snaps.back()?;
        Some(reclaim::reclaim(last, &w, own_pid))
    }
}

/// Texto de uma linha, sem GTK.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateText {
    pub pid: u32,
    pub link: SessionLink,
    pub name: String,
    pub meta: String,
    pub why: String,
    pub memory: String,
    pub memory_tooltip: String,
    /// Fatia do total recuperavel, 0..=1.
    pub share: f64,
    pub a11y: String,
}

/// O texto de cada candidata, na ordem do core. O nome e o caminho vem da
/// mesma [`Row`] da lista, para a sessao se chamar igual nas duas paginas.
pub fn texts(r: &Reclaim, snap: &Snapshot, home: Option<&str>) -> Vec<CandidateText> {
    let source = snap.machine.io_stall_source;
    r.sessions
        .iter()
        .map(|c| {
            let row = snap
                .sessions
                .iter()
                .find(|s| s.pid == c.pid)
                .map(|s| Row::from_session(s, source, home));
            let name = row
                .as_ref()
                .map_or_else(|| sanitize(&c.agent).into_owned(), |r| r.name.clone());
            let cwd = row.as_ref().map(|r| r.cwd.clone()).unwrap_or_default();
            let agent = sanitize(&c.agent);
            let meta = if cwd.is_empty() {
                format!("{agent} · pid {}", c.pid)
            } else {
                format!("{agent} · {cwd} · pid {}", c.pid)
            };
            let why = match (c.idle_by, c.max_cpu_pct) {
                (IdleBy::Overlay, _) => "Idle by its own status".to_string(),
                (IdleBy::Cpu, Some(m)) => {
                    format!("Quiet: CPU at most {} for {DEFAULT_WINDOW_S} s", cpu_pct(m))
                }
                (IdleBy::Cpu, None) => format!("Quiet: CPU under {IDLE_MAX_CPU_PCT:.0} %"),
            };
            let star = if c.memory_partial {
                " (some processes unreadable)"
            } else {
                ""
            };
            let memory = bytes(c.memory_bytes() as f64);
            let memory_tooltip = format!(
                "PSS {} + swap PSS {}{star}",
                bytes(c.pss_bytes as f64),
                c.swap_pss_bytes
                    .map_or_else(|| DASH.to_string(), |b| bytes(b as f64)),
            );
            let share = if r.total_bytes > 0 {
                (c.memory_bytes() as f64 / r.total_bytes as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let a11y = format!("{name}, {meta}, {why}, frees {memory}");
            CandidateText {
                pid: c.pid,
                link: SessionLink {
                    pid: c.pid,
                    starttime: snap
                        .sessions
                        .iter()
                        .find(|s| s.pid == c.pid)
                        .and_then(|s| s.starttime),
                    overlay_id: snap
                        .sessions
                        .iter()
                        .find(|s| s.pid == c.pid)
                        .and_then(|s| s.overlay.as_ref())
                        .and_then(|o| o.agent_session_id.clone()),
                },
                name,
                meta,
                why,
                memory,
                memory_tooltip,
                share,
                a11y,
            }
        })
        .collect()
}

/// Linha de resumo: "in 4 idle sessions · watched 10 s".
pub fn summary(r: &Reclaim) -> (String, String) {
    let n = r.sessions.len();
    (
        bytes(r.total_bytes as f64),
        format!(
            "in {n} idle session{} · watched {DEFAULT_WINDOW_S} s",
            if n == 1 { "" } else { "s" }
        ),
    )
}

struct RowWidgets {
    row: gtk::ListBoxRow,
    name: gtk::Label,
    meta: gtk::Label,
    why: gtk::Label,
    memory: gtk::Label,
    meter: gtk::LevelBar,
    button: gtk::Button,
    view: gtk::Button,
    link: Rc<RefCell<Option<SessionLink>>>,
}

fn label(classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(0.0);
    for c in classes {
        l.add_css_class(c);
    }
    l
}

fn set_label(l: &gtk::Label, t: &str) {
    if l.label() != t {
        l.set_label(t);
    }
}

impl RowWidgets {
    fn new(open: OpenSession) -> Self {
        let name = label(&["session-name"]);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let meta = label(&["session-meta"]);
        meta.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        let why = label(&["session-meta"]);
        why.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let left = gtk::Box::new(gtk::Orientation::Vertical, 2);
        left.set_hexpand(true);
        left.append(&name);
        left.append(&meta);
        left.append(&why);

        let memory = label(&["reclaim-memory", "numeric"]);
        memory.set_xalign(1.0);
        let meter = gtk::LevelBar::new();
        meter.set_min_value(0.0);
        meter.set_max_value(1.0);
        for n in [
            gtk::LEVEL_BAR_OFFSET_LOW,
            gtk::LEVEL_BAR_OFFSET_HIGH,
            gtk::LEVEL_BAR_OFFSET_FULL,
        ] {
            meter.remove_offset_value(Some(n));
        }
        meter.add_css_class("meter");
        meter.set_size_request(48, -1);
        meter.set_halign(gtk::Align::End);
        meter.set_accessible_role(gtk::AccessibleRole::Presentation);
        let button = gtk::Button::with_label(&format!("{}…", actions_ui::label(Action::Term)));
        button.add_css_class("reclaim-end");
        button.set_halign(gtk::Align::End);
        let right = gtk::Box::new(gtk::Orientation::Vertical, 4);
        right.set_valign(gtk::Align::Center);
        right.append(&memory);
        right.append(&meter);
        let view = gtk::Button::with_label("View session");
        view.set_size_request(44, 44);
        let link: Rc<RefCell<Option<SessionLink>>> = Rc::default();
        let target = link.clone();
        view.connect_clicked(move |button| {
            if let Some(link) = target.borrow().as_ref() {
                open(link, button);
            }
        });
        right.append(&view);
        right.append(&button);

        let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        b.add_css_class("reclaim-row");
        b.append(&left);
        b.append(&right);
        let row = gtk::ListBoxRow::builder()
            .child(&b)
            .activatable(false)
            .selectable(false)
            .build();
        Self {
            row,
            name,
            meta,
            why,
            memory,
            meter,
            button,
            view,
            link,
        }
    }

    fn set(&self, t: &CandidateText) {
        self.link.replace(Some(t.link.clone()));
        self.view
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "View session {}",
                t.name
            ))]);
        set_label(&self.name, &t.name);
        self.name.set_tooltip_text(Some(&t.name));
        set_label(&self.meta, &t.meta);
        self.meta.set_tooltip_text(Some(&t.meta));
        set_label(&self.why, &t.why);
        set_label(&self.memory, &t.memory);
        self.memory.set_tooltip_text(Some(&t.memory_tooltip));
        if (self.meter.value() - t.share).abs() > 1e-9 {
            self.meter.set_value(t.share);
        }
        self.row
            .update_property(&[gtk::accessible::Property::Label(&t.a11y)]);
        let desc = format!("Terminate {}", t.name);
        self.button
            .update_property(&[gtk::accessible::Property::Description(&desc)]);
        // Set the typed target before resolving the action to avoid GTK
        // briefly binding its required (sbus) parameter to a NULL target.
        self.button
            .set_action_target_value(Some(&actions_ui::action_target(
                Action::Term,
                Target::Session(t.pid),
                &t.name,
            )));
        self.button
            .set_action_name(Some(&format!("win.{}", actions_ui::WIN_ACTION)));
    }
}

pub struct ReclaimPage {
    pub widget: gtk::Box,
    stack: gtk::Stack,
    watching: adw::StatusPage,
    total: gtk::Label,
    context: gtk::Label,
    list: gtk::ListBox,
    open: RefCell<OpenSession>,
    pub scroll: gtk::ScrolledWindow,
    rows: RefCell<Vec<(u32, RowWidgets)>>,
    ring: RefCell<Ring>,
    own: u32,
    shown: RefCell<Vec<CandidateText>>,
    /// Pids da primeira janela cheia, uma vez, para scripts/reclaim-parity.sh.
    report: Cell<bool>,
}

impl ReclaimPage {
    pub fn new() -> Self {
        let caption = label(&["detail-caption"]);
        caption.set_label("Reclaimable");
        let total = label(&["reclaim-total", "numeric"]);
        total.set_label(DASH);
        let context = label(&["detail-meta", "numeric"]);
        context.set_ellipsize(gtk::pango::EllipsizeMode::End);
        context.set_hexpand(true);
        let line = adw::WrapBox::builder()
            .child_spacing(12)
            .line_spacing(2)
            .build();
        line.append(&caption);
        line.append(&total);
        line.append(&context);
        let note = label(&["detail-note-inline"]);
        note.set_label(&format!(
            "Idle: the agent reports idle, or its CPU stayed under {IDLE_MAX_CPU_PCT:.0} % of a \
             core for {DEFAULT_WINDOW_S} s. Busy and blocked sessions never show here. Nothing \
             ends until you confirm."
        ));
        note.set_wrap(true);
        note.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        let face = gtk::Box::new(gtk::Orientation::Vertical, 6);
        face.add_css_class("detail-face");
        face.append(&line);
        face.append(&note);

        let watching = adw::StatusPage::builder()
            .icon_name("edit-clear-all-symbolic")
            .title("Watching for Idle Sessions")
            .build();
        watching.add_css_class("compact");
        let empty = adw::StatusPage::builder()
            .icon_name("edit-clear-all-symbolic")
            .title("No Idle Sessions")
            .description(format!(
                "Every session is busy, blocked or used CPU in the last {DEFAULT_WINDOW_S} s. \
                 Idle sessions appear here with the memory they hold."
            ))
            .build();
        empty.add_css_class("compact");

        let list = gtk::ListBox::new();
        list.add_css_class("reclaim-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        list.update_property(&[gtk::accessible::Property::Label("Idle sessions")]);
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        scrolled.add_css_class("detail-table");

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&watching, Some(STACK_WATCHING));
        stack.add_named(&empty, Some(STACK_EMPTY));
        stack.add_named(&scrolled, Some(STACK_LIST));

        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("session-detail");
        widget.add_css_class("reclaim-page");
        widget.append(&face);
        widget.append(&stack);

        let page = Self {
            widget,
            stack,
            watching,
            total,
            context,
            list,
            open: RefCell::new(Rc::new(|_, _| {})),
            scroll: scrolled,
            rows: RefCell::default(),
            ring: RefCell::default(),
            own: std::process::id(),
            shown: RefCell::default(),
            report: Cell::new(true),
        };
        page.set_watching(0);
        page
    }

    pub fn connect_open(&self, open: impl Fn(&SessionLink, &gtk::Button) + 'static) {
        self.open.replace(Rc::new(open));
    }

    fn set_watching(&self, have: usize) {
        let left = WINDOW_TICKS.saturating_sub(have);
        self.watching.set_description(Some(&format!(
            "A session counts as idle after {DEFAULT_WINDOW_S} s of CPU readings. \
             {left} s to go."
        )));
        set_label(&self.total, DASH);
        set_label(&self.context, "watching CPU");
        if self.stack.visible_child_name().as_deref() != Some(STACK_WATCHING) {
            self.stack.set_visible_child_name(STACK_WATCHING);
        }
    }

    /// Todo tick, visivel ou nao: a janela precisa estar cheia quando a
    /// pagina abrir.
    pub fn observe(&self, snap: &Arc<Snapshot>) {
        self.ring.borrow_mut().push(snap);
    }

    /// Redesenha com a janela atual. So quando a pagina esta visivel.
    pub fn refresh(&self, home: Option<&str>) {
        let ring = self.ring.borrow();
        let Some(r) = ring.reclaim(Some(self.own)) else {
            self.set_watching(ring.len());
            return;
        };
        let Some(snap) = ring.snaps.back().cloned() else {
            return;
        };
        drop(ring);
        if self.report.replace(false) {
            let pids: Vec<String> = r.sessions.iter().map(|c| c.pid.to_string()).collect();
            println!(
                "reclaim_pids={} reclaim_total={}",
                pids.join(","),
                r.total_bytes
            );
        }
        let (total, context) = summary(&r);
        set_label(&self.total, &total);
        set_label(&self.context, &context);
        let texts = texts(&r, &snap, home);
        // `shown` comeca vazio: sem a checagem da pilha, a primeira janela
        // cheia sem ociosas ficava presa no "watching" (achado na captura
        // do vazio).
        let watching = self.stack.visible_child_name().as_deref() == Some(STACK_WATCHING);
        if !watching && *self.shown.borrow() == texts {
            return;
        }
        let page = if texts.is_empty() {
            STACK_EMPTY
        } else {
            STACK_LIST
        };
        let mut rows = self.rows.borrow_mut();
        let same_order =
            rows.len() == texts.len() && rows.iter().zip(&texts).all(|((pid, _), t)| *pid == t.pid);
        if !same_order {
            // A lista muda de membros raramente (uma sessao fica ociosa ou
            // volta): refaz as linhas so entao, na ordem do core.
            for (_, w) in rows.drain(..) {
                self.list.remove(&w.row);
            }
            for t in &texts {
                let w = RowWidgets::new(self.open.borrow().clone());
                self.list.append(&w.row);
                rows.push((t.pid, w));
            }
        }
        for ((_, w), t) in rows.iter().zip(&texts) {
            w.set(t);
        }
        if self.stack.visible_child_name().as_deref() != Some(page) {
            self.stack.set_visible_child_name(page);
        }
        self.shown.replace(texts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::adapters::AgentRole;
    use agentcenter_core::overlay::{OverlayStatus, SessionOverlay};
    use agentcenter_core::session::ProcessMetrics;
    use agentcenter_core::{IoStallSource, Machine, Session};

    fn snap(interval_ms: u64, sessions: Vec<Session>) -> Arc<Snapshot> {
        Arc::new(Snapshot {
            schema_version: agentcenter_core::snapshot::SCHEMA_VERSION,
            taken_at_ms: 0,
            interval_ms,
            machine: Machine {
                cores: 16,
                cpu_pct: None,
                io_stall_source: IoStallSource::Delayacct,
                processes: 0,
                session_processes: 0,
                mem_total_bytes: None,
                mem_available_bytes: None,
                swap_total_bytes: None,
                swap_free_bytes: None,
                load_avg: None,
                psi: None,
            },
            sessions,
            summary: Default::default(),
        })
    }

    fn session(pid: u32, cpu: f64, pss: u64, status: Option<OverlayStatus>) -> Session {
        let p = ProcessMetrics {
            pid,
            ppid: 1,
            comm: "claude".into(),
            state: 'S',
            uid: 1000,
            threads: 1,
            cpu_pct: Some(cpu),
            rss_bytes: Some(pss),
            pss_bytes: Some(pss),
            swap_bytes: Some(0),
            swap_pss_bytes: Some(0),
            read_bps: Some(0.0),
            write_bps: Some(0.0),
            rchar_bps: Some(0.0),
            wchar_bps: Some(0.0),
            stall_pct: Some(0.0),
            threads_d: None,
            voluntary_ctxt_switches: 0,
            nonvoluntary_ctxt_switches: 0,
        };
        let mut s = Session::aggregate("claude".into(), AgentRole::Agent, pid, 16, vec![p]);
        s.overlay = status.map(|st| SessionOverlay {
            status: Some(st),
            ..Default::default()
        });
        s
    }

    #[test]
    fn kernel_link_survives_missing_overlay_but_rejects_pid_reuse() {
        let mut s = session(10, 0.0, 1, None);
        s.starttime = Some(123);
        let link = SessionLink {
            pid: 10,
            starttime: Some(123),
            overlay_id: None,
        };
        assert!(link.matches(&snap(1000, vec![s.clone()])));
        s.starttime = Some(124);
        assert!(!link.matches(&snap(1000, vec![s.clone()])));
        s.starttime = None;
        assert!(!link.matches(&snap(1000, vec![s])));
        assert!(!link.matches(&snap(1000, vec![])));
    }

    /// A native session can change without restarting its hosting process.
    #[test]
    fn cc2_regression_native_session_replacement_is_not_the_same_link() {
        let mut s = session(10, 0.0, 1, Some(OverlayStatus::Idle));
        s.starttime = Some(123);
        s.overlay.as_mut().unwrap().agent_session_id = Some("original".into());
        let link = SessionLink {
            pid: 10,
            starttime: Some(123),
            overlay_id: Some("original".into()),
        };
        assert!(link.matches(&snap(1000, vec![s.clone()])));
        s.overlay.as_mut().unwrap().agent_session_id = Some("replacement".into());
        assert!(
            !link.matches(&snap(1000, vec![s])),
            "same PID/starttime must not silently navigate to a different native session"
        );
    }

    #[test]
    fn navigation_link_rejects_missing_or_replaced_overlay() {
        let mut s = session(10, 0.0, 1, Some(OverlayStatus::Idle));
        s.overlay.as_mut().unwrap().agent_session_id = Some("original".into());
        let link = SessionLink {
            pid: 10,
            starttime: None,
            overlay_id: Some("original".into()),
        };
        assert!(link.matches(&snap(1000, vec![s.clone()])));
        s.overlay.as_mut().unwrap().agent_session_id = Some("replacement".into());
        assert!(!link.matches(&snap(1000, vec![s])));
        assert!(!link.matches(&snap(1000, vec![])));
        assert!(
            !SessionLink {
                pid: 10,
                starttime: None,
                overlay_id: None
            }
            .matches(&snap(1000, vec![session(10, 0.0, 1, None)]))
        );
    }

    /// Overlay equality alone must not navigate to another PID or accept absent identity.
    #[test]
    fn navigation_requires_both_pid_and_nonempty_overlay_identity() {
        let mut s = session(20, 0.0, 1, Some(OverlayStatus::Idle));
        s.overlay.as_mut().unwrap().agent_session_id = Some("same".into());
        let link = SessionLink {
            pid: 10,
            starttime: None,
            overlay_id: Some("same".into()),
        };
        assert!(!link.matches(&snap(1000, vec![s.clone()])));
        s.pid = 10;
        assert!(link.matches(&snap(1000, vec![s.clone()])));
        for id in [None, Some(String::new())] {
            s.overlay.as_mut().unwrap().agent_session_id = id.clone();
            assert!(!link.matches(&snap(1000, vec![s.clone()])));
            assert!(
                !SessionLink {
                    pid: 10,
                    starttime: None,
                    overlay_id: id
                }
                .matches(&snap(1000, vec![s.clone()]))
            );
        }
        s.overlay = None;
        assert!(!link.matches(&snap(1000, vec![s])));
        // This checks overlay navigation only, not kernel starttime/PID-reuse safety.
    }

    #[test]
    fn ring_waits_for_a_full_window_and_then_slides() {
        let quiet = vec![session(10, 0.1, 1 << 30, None)];
        let mut ring = Ring::default();
        ring.push(&snap(0, quiet.clone()));
        assert_eq!(ring.len(), 0, "the tick without rates does not count");
        for _ in 0..WINDOW_TICKS - 1 {
            ring.push(&snap(1000, quiet.clone()));
        }
        assert!(
            ring.reclaim(None).is_none(),
            "9 of 10 ticks: still watching"
        );
        ring.push(&snap(1000, quiet.clone()));
        assert_eq!(ring.reclaim(None).unwrap().sessions.len(), 1);
        // Um tick com CPU alta fica na janela por 10 ticks, depois sai.
        ring.push(&snap(1000, vec![session(10, 30.0, 1 << 30, None)]));
        for i in 0..WINDOW_TICKS {
            assert!(ring.reclaim(None).unwrap().sessions.is_empty(), "tick {i}");
            ring.push(&snap(1000, quiet.clone()));
        }
        assert_eq!(ring.reclaim(None).unwrap().sessions.len(), 1);
        assert_eq!(ring.len(), WINDOW_TICKS);
    }

    #[test]
    fn ring_matches_the_core_rule_over_the_same_ticks() {
        // A mesma sequencia que o CLI veria: o reclaim da janela e o do core.
        let ticks: Vec<Vec<Session>> = (0..WINDOW_TICKS)
            .map(|i| {
                vec![
                    session(10, 0.2, 2 << 30, None),
                    session(20, if i == 3 { 5.0 } else { 0.0 }, 3 << 30, None),
                    session(30, 40.0, 1 << 30, Some(OverlayStatus::Idle)),
                    session(40, 0.0, 4 << 30, Some(OverlayStatus::Busy)),
                ]
            })
            .collect();
        let mut ring = Ring::default();
        let mut w = CpuWindow::new();
        for t in &ticks {
            let s = snap(1000, t.clone());
            ring.push(&s);
            w.observe(&s);
        }
        let last = snap(1000, ticks[WINDOW_TICKS - 1].clone());
        assert_eq!(
            ring.reclaim(Some(1)),
            Some(reclaim::reclaim(&last, &w, Some(1)))
        );
        let pids: Vec<u32> = ring
            .reclaim(Some(1))
            .unwrap()
            .sessions
            .iter()
            .map(|c| c.pid)
            .collect();
        assert_eq!(pids, [10, 30]);
    }

    #[test]
    fn texts_say_why_and_how_much_with_the_list_name() {
        let mut s = session(10, 0.25, 3 << 30, None);
        s.cwd = Some("/home/dev/src/kiln".into());
        let o = session(30, 50.0, 1 << 30, Some(OverlayStatus::Idle));
        let ticks = vec![s, o];
        let mut ring = Ring::default();
        for _ in 0..WINDOW_TICKS {
            ring.push(&snap(1000, ticks.clone()));
        }
        let r = ring.reclaim(None).unwrap();
        let last = snap(1000, ticks);
        let t = texts(&r, &last, Some("/home/dev"));
        assert_eq!(t[0].pid, 10);
        assert_eq!(t[0].meta, "claude · ~/src/kiln · pid 10");
        assert!(t[0].why.starts_with("Quiet: CPU at most"), "{}", t[0].why);
        assert!((t[0].share - 0.75).abs() < 1e-9);
        assert_eq!(t[1].why, "Idle by its own status");
        assert_eq!(summary(&r).1, "in 2 idle sessions · watched 10 s");
    }
}

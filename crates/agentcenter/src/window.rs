//! Janela principal. Recebe um frame por tick e so atualiza modelo e rotulos:
//! nenhum widget e criado depois da construcao, salvo as linhas de sessao nova
//! e as de processo novo no detalhe.
//!
//! Layout (C11): a faixa da maquina por cima de um `AdwNavigationSplitView`,
//! lista na barra lateral e detalhe da selecionada no conteudo. Abaixo de
//! [`COLLAPSE_MAX_SP`] o split recolhe: a lista ocupa a janela e o detalhe e
//! empurrado por cima dela, com o proprio cabecalho (voltar), sem a faixa.
//!
//! C12: duas paginas num `AdwViewStack`, Sessions (a de cima) e Reclaim
//! (reclaim_page.rs), trocadas por um `AdwInlineViewSwitcher` no comeco do
//! cabecalho; o titulo com as contagens fica. Toasts das acoes num
//! `AdwToastOverlay` em volta de tudo; os menus chamam `win.act`
//! (actions_ui.rs).

use crate::actions_ui::Actions;
use crate::dashboard::Dashboard;
use crate::detail::Detail;
use crate::machine_header::MachineHeader;
use crate::prefs::{Prefs, WindowSize};
use crate::reclaim_page::ReclaimPage;
use crate::session_list::{MIN_WIDTH_PX, SessionList, SessionModel, scrolled};
use adw::prelude::*;
use agentcenter_core::procfs::ReadError;
use agentcenter_core::{Frame, History, IoStallSource, Snapshot};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

/// Largura (sp) ate a qual o split fica recolhido: lista e detalhe lado a
/// lado so a partir de 840.
pub const COLLAPSE_MAX_SP: f64 = 839.0;

/// Abre o detalhe de uma sessao na partida (medicao e captura do detalhe):
/// `busiest` e a sessao com mais processos, ou um pid.
pub const DETAIL_ENV: &str = "AGENTCENTER_DETAIL";

/// Com `AGENTCENTER_DETAIL`, `1` volta do detalhe logo depois de abri-lo, pela
/// acao `navigation.pop` (a de Esc e Alt+Esquerda), com o foco visivel, e
/// imprime onde o foco caiu (`back_focus=`): a captura e o capture-gui.sh
/// conferem a volta pelo teclado em 360 px sem injetar tecla.
pub const DETAIL_BACK_ENV: &str = "AGENTCENTER_DETAIL_BACK";

/// `reclaim` abre a janela na pagina Reclaim (captura, medicao, paridade).
pub const PAGE_ENV: &str = "AGENTCENTER_PAGE";
pub const VIEW_SESSIONS: &str = "sessions";
pub const VIEW_RECLAIM: &str = "reclaim";

/// Largura (sp) ate a qual o seletor de paginas mostra so os icones.
pub const SWITCHER_ICONS_MAX_SP: f64 = 520.0;

const PAGE_LIST: &str = "list";
const PAGE_EMPTY: &str = "empty";
const PAGE_ERROR: &str = "error";

const ENABLE_NOW: &str = "sudo sysctl kernel.task_delayacct=1";
const ENABLE_BOOT: &str =
    "echo 'kernel.task_delayacct = 1' | sudo tee /etc/sysctl.d/90-task-delayacct.conf";

pub struct Window {
    pub window: adw::ApplicationWindow,
    title: adw::WindowTitle,
    banner: adw::Banner,
    header: MachineHeader,
    model: RefCell<SessionModel>,
    list: Rc<SessionList>,
    detail: Rc<Detail>,
    dashboard: Rc<Dashboard>,
    split: adw::NavigationSplitView,
    last: Rc<RefCell<Option<Arc<Snapshot>>>>,
    last_history: Rc<RefCell<Option<Arc<History>>>>,
    open_at_start: Cell<Option<String>>,
    stack: gtk::Stack,
    error: adw::StatusPage,
    home: Option<String>,
    view: adw::ViewStack,
    reclaim: Rc<ReclaimPage>,
    actions: Rc<Actions>,
    probed: Cell<bool>,
}

/// Qual sessao `AGENTCENTER_DETAIL` pede: a de mais processos, ou um pid.
pub fn pick_detail(want: &str, snap: &Snapshot) -> Option<u32> {
    match want.trim() {
        "busiest" => snap
            .sessions
            .iter()
            .max_by(|a, b| a.procs.cmp(&b.procs).then_with(|| b.pid.cmp(&a.pid)))
            .map(|s| s.pid),
        pid => pid
            .parse()
            .ok()
            .filter(|p| snap.sessions.iter().any(|s| s.pid == *p)),
    }
}

impl Window {
    /// `select`: pede ao sampler a sessao selecionada (smaps_rollup e `task/`
    /// todo tick so nela); no modo fixture nao faz nada.
    /// `live`: dados do sampler desta maquina; no modo fixture as acoes
    /// ficam desligadas (os pids sao de outra maquina).
    /// `prefs`: ordem da lista e tamanho da janela (C19); sem schema, os
    /// padroes e nada gravado.
    pub fn new(
        app: &adw::Application,
        live: bool,
        prefs: &Prefs,
        select: impl Fn(Option<u32>) + 'static,
    ) -> Self {
        let title = adw::WindowTitle::new("AgentCenter", "Reading sessions…");
        let headerbar = adw::HeaderBar::builder().title_widget(&title).build();
        let view = adw::ViewStack::new();
        view.set_vexpand(true);
        let switcher = adw::InlineViewSwitcher::builder()
            .stack(&view)
            .valign(gtk::Align::Center)
            .build();
        switcher.add_css_class("flat");
        switcher.set_tooltip_text(Some("Page"));
        headerbar.pack_start(&switcher);

        let banner = adw::Banner::builder()
            .title("I/O stall is estimated from threads waiting on disk")
            .button_label("Get Exact Stall")
            .revealed(false)
            .build();

        let header = MachineHeader::new();
        let model = SessionModel::default();
        let list = Rc::new(SessionList::new(&model));
        list.set_sort_key(prefs.sort_key());
        let p = prefs.clone();
        list.connect_sort_changed(move |k| p.set_sort_key(k));
        let detail = Rc::new(Detail::new(live));
        let home = std::env::var("HOME").ok();

        let dashboard = Rc::new(Dashboard::new());
        let dashboard_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&dashboard.widget)
            .build();
        let content = gtk::Stack::new();
        content.add_named(&dashboard_scroll, Some("overview"));
        // Keep ONE NavigationPage as the split content. Nesting detail.page
        // inside another page disconnects navigation.pop and back-focus restore.
        detail.page.set_child(gtk::Widget::NONE);
        content.add_named(&detail.toolbar, Some("detail"));
        content.set_visible_child_name("overview");
        let reclaim_back = gtk::Button::with_label("Back to Reclaim");
        reclaim_back.set_visible(false);
        reclaim_back.set_halign(gtk::Align::Start);
        reclaim_back.set_size_request(44, 44);
        let detail_body = gtk::Box::new(gtk::Orientation::Vertical, 8);
        detail_body.append(&reclaim_back);
        content.set_vexpand(true);
        detail_body.append(&content);
        detail.page.set_child(Some(&detail_body));
        let disclosure = gtk::Expander::builder()
            .label("Session resource history")
            .visible(false)
            .build();
        let rack = gtk::Box::new(gtk::Orientation::Vertical, 0);
        rack.append(&disclosure);
        rack.append(&scrolled(&list));
        let sidebar = adw::NavigationPage::builder()
            .title("Sessions")
            .child(&rack)
            .build();
        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar)
            .content(&detail.page)
            .min_sidebar_width(f64::from(MIN_WIDTH_PX) + 40.0)
            .max_sidebar_width(620.0)
            .sidebar_width_fraction(0.44)
            .build();

        let empty = adw::StatusPage::builder()
            .icon_name("utilities-system-monitor-symbolic")
            .title("No Agent Sessions")
            .description(
                "Claude Code, Codex, pi and other coding agents appear here as soon \
                 as they start.",
            )
            .build();
        let error = adw::StatusPage::builder()
            .icon_name("dialog-error-symbolic")
            .title("Cannot Read Processes")
            .build();

        let stack = gtk::Stack::new();
        // O split nao propaga o expand dos filhos: sem isto lista e detalhe
        // ficavam na altura natural, com a janela vazia embaixo.
        stack.set_vexpand(true);
        stack.add_named(&split, Some(PAGE_LIST));
        stack.add_named(&empty, Some(PAGE_EMPTY));
        stack.add_named(&error, Some(PAGE_ERROR));
        stack.set_visible_child_name(PAGE_LIST);

        let reclaim = Rc::new(ReclaimPage::new());
        view.add_titled_with_icon(
            &stack,
            Some(VIEW_SESSIONS),
            "Sessions",
            "utilities-terminal-symbolic",
        );
        view.add_titled_with_icon(
            &reclaim.widget,
            Some(VIEW_RECLAIM),
            "Reclaim",
            "edit-clear-all-symbolic",
        );
        if std::env::var(PAGE_ENV).is_ok_and(|v| v.trim() == VIEW_RECLAIM) {
            view.set_visible_child_name(VIEW_RECLAIM);
        }

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&banner);
        body.append(&header.widget);
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        body.append(&view);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&headerbar);
        toolbar.set_content(Some(&body));
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&toolbar));

        // Fixture captures must not inherit or overwrite the owner's maximized state.
        let capturing = std::env::var_os("AGENTCENTER_SCREENSHOT").is_some();
        let size = prefs.window_size();
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("AgentCenter")
            .default_width(size.width)
            .default_height(size.height)
            .maximized(size.maximized && !capturing)
            .width_request(360)
            .height_request(294)
            .content(&toasts)
            .build();

        // O tamanho e gravado so ao fechar, nao a cada passo de um arraste.
        let p = prefs.clone();
        window.connect_close_request(move |w| {
            if capturing {
                return gtk::glib::Propagation::Proceed;
            }
            let (width, height) = w.default_size();
            p.set_window_size(WindowSize {
                width,
                height,
                maximized: w.is_maximized(),
            });
            gtk::glib::Propagation::Proceed
        });

        // Com varios breakpoints valendo, o libadwaita aplica so o ultimo: o
        // estreito repete o recolhimento do split.
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            COLLAPSE_MAX_SP,
            adw::LengthUnit::Sp,
        ));
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(bp);
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            SWITCHER_ICONS_MAX_SP,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        narrow.add_setter(
            &switcher,
            "display-mode",
            Some(&adw::InlineViewSwitcherDisplayMode::Icons.to_value()),
        );
        window.add_breakpoint(narrow);

        let (ds, disc, dash) = (
            dashboard_scroll.clone(),
            disclosure.clone(),
            dashboard.clone(),
        );
        split.connect_collapsed_notify(move |sp| {
            if sp.is_collapsed() {
                ds.set_child(gtk::Widget::NONE);
                disc.set_child(Some(&dash.widget));
                disc.set_visible(true);
            } else {
                disc.set_child(gtk::Widget::NONE);
                disc.set_visible(false);
                ds.set_child(Some(&dash.widget));
            }
        });

        let actions = Actions::new(&window, &toasts, live);
        actions.install();

        // Recolhido com o detalhe na frente: o cabecalho da pagina (voltar e
        // nome da sessao) no lugar do da janela, e sem a faixa da maquina, que
        // e da lista. Aberto: um cabecalho so, o da janela.
        let chrome = {
            let (toolbar, band, detail, split) = (
                toolbar.clone(),
                header.widget.clone(),
                detail.clone(),
                split.clone(),
            );
            move || {
                let collapsed = split.is_collapsed();
                let on_detail = collapsed && split.shows_content();
                detail.toolbar.set_reveal_top_bars(collapsed);
                toolbar.set_reveal_top_bars(!on_detail);
                band.set_visible(!on_detail);
            }
        };
        let chrome = Rc::new(chrome);
        chrome();
        let c = chrome.clone();
        split.connect_collapsed_notify(move |_| c());
        // Voltou do detalhe (Esc, Alt+Esquerda, botao voltar): o foco volta
        // para a linha da sessao, nao para o topo da janela. So no fim da
        // transicao (`shown` da pagina da lista): antes dele o AdwNavigationView
        // ainda poe o foco no primeiro focavel da pagina, o seletor de ordem
        // (achado pela sonda AGENTCENTER_DETAIL_BACK). Sem animacao (reduzir
        // movimento, gtk-enable-animations=false) a ordem inverte: o `shown`
        // sai antes do notify, entao o notify tambem foca, num idle.
        let returning = Rc::new(Cell::new(false));
        // Deferred focus belongs to the current destination, not the page
        // that scheduled it: a Reclaim return can hide Sessions before idle.
        let focus_list: Rc<dyn Fn()> = {
            let (view, split, list) = (view.clone(), split.clone(), list.clone());
            Rc::new(move || {
                if view.visible_child_name().as_deref() == Some(VIEW_SESSIONS)
                    && split.is_collapsed()
                    && !split.shows_content()
                {
                    list.focus_selected();
                }
            })
        };
        let (c, r, focus) = (chrome.clone(), returning.clone(), focus_list.clone());
        split.connect_show_content_notify(move |split| {
            c();
            if split.is_collapsed() && !split.shows_content() {
                r.set(true);
                let focus = focus.clone();
                gtk::glib::idle_add_local_once(move || focus());
            }
        });
        let (focus, r) = (focus_list, returning);
        sidebar.connect_shown(move |_| {
            if r.replace(false) {
                // No idle: o AdwNavigationView ainda mexe no foco no `shown`.
                let focus = focus.clone();
                gtk::glib::idle_add_local_once(move || focus());
            }
        });

        let last: Rc<RefCell<Option<Arc<Snapshot>>>> = Rc::default();
        let last_history: Rc<RefCell<Option<Arc<History>>>> = Rc::default();
        let (d, lst, lh, h) = (
            detail.clone(),
            last.clone(),
            last_history.clone(),
            home.clone(),
        );
        list.connect_selected(move |pid| {
            content.set_visible_child_name(if pid.is_some() { "detail" } else { "overview" });
            d.show(
                pid,
                lst.borrow().as_deref(),
                lh.borrow().as_ref(),
                h.as_deref(),
            );
            select(pid);
        });
        // Keep the original Reclaim widgets alive: return restores their scroll
        // and the originating button, rather than reconstructing a candidate list.
        let origin: Rc<RefCell<Option<(f64, gtk::Button)>>> = Rc::default();
        let back: Rc<dyn Fn()> = {
            let (origin, reclaim, view, button, split) = (
                origin.clone(),
                reclaim.clone(),
                view.clone(),
                reclaim_back.clone(),
                split.clone(),
            );
            Rc::new(move || {
                let Some((position, focus)) = origin.borrow_mut().take() else {
                    return;
                };
                button.set_visible(false);
                // The explicit button must leave detail just like native pop;
                // otherwise narrow chrome remains hidden on the Reclaim page.
                split.set_show_content(false);
                view.set_visible_child_name(VIEW_RECLAIM);
                let scroll = reclaim.scroll.clone();
                let view = view.clone();
                gtk::glib::idle_add_local_once(move || {
                    if view.visible_child_name().as_deref() != Some(VIEW_RECLAIM) {
                        return;
                    }
                    if focus.root().is_some() {
                        focus.grab_focus();
                    }
                    scroll.vadjustment().set_value(position);
                });
            })
        };
        let return_to_reclaim = back.clone();
        reclaim_back.connect_clicked(move |_| return_to_reclaim());
        let (return_to_reclaim, pending) = (back, origin.clone());
        split.connect_show_content_notify(move |sp| {
            if !sp.shows_content() && pending.borrow().is_some() {
                return_to_reclaim();
            }
        });
        let (lst, list_for_open, sp, view_for_open, origin_for_open, back_button, toast) = (
            last.clone(),
            list.clone(),
            split.clone(),
            view.clone(),
            origin,
            reclaim_back,
            toasts.clone(),
        );
        let scroll = reclaim.scroll.clone();
        reclaim.connect_open(move |link, button| {
            let snapshot = lst.borrow();
            if !snapshot.as_deref().is_some_and(|s| {
                if link.starttime.is_some() {
                    link.matches(s)
                } else {
                    !live
                        && s.sessions
                            .iter()
                            .any(|s| s.pid == link.pid && s.starttime.is_none())
                }
            }) {
                let message = if link.overlay_id.is_none() {
                    "Session identity unavailable. Return to Sessions to inspect it."
                } else {
                    "Session ended or changed. Refresh Reclaim and try again."
                };
                toast.add_toast(adw::Toast::new(message));
                return;
            }
            if !list_for_open.reveal_pid(link.pid) {
                toast.add_toast(adw::Toast::new(
                    "Session is not available in the session list.",
                ));
                return;
            }
            origin_for_open.replace(Some((scroll.vadjustment().value(), button.clone())));
            back_button.set_visible(true);
            view_for_open.set_visible_child_name(VIEW_SESSIONS);
            sp.set_show_content(true);
            back_button.grab_focus();
        });

        let (filtered_list, filtered_view, filtered_stack, filtered_split) =
            (list.clone(), view.clone(), stack.clone(), split.clone());
        header.connect_filter(move |filter| {
            filtered_list.set_filter(filter);
            filtered_view.set_visible_child_name(VIEW_SESSIONS);
            filtered_stack.set_visible_child_name(PAGE_LIST);
            filtered_split.set_show_content(false);
            let list = filtered_list.clone();
            gtk::glib::idle_add_local_once(move || list.focus_filtered());
        });

        let sp = split.clone();
        list.connect_open(move |_| {
            if sp.is_collapsed() {
                sp.set_show_content(true);
            }
        });

        let parent = window.clone();
        banner.connect_button_clicked(move |_| delayacct_dialog().present(Some(&parent)));

        // A pagina Reclaim so redesenha visivel; ao abrir, com a janela atual.
        let (r, h) = (reclaim.clone(), home.clone());
        view.connect_visible_child_name_notify(move |v| {
            if v.visible_child_name().as_deref() == Some(VIEW_RECLAIM) {
                r.refresh(h.as_deref());
            }
        });

        Self {
            window,
            title,
            banner,
            header,
            model: RefCell::new(model),
            list,
            detail,
            dashboard,
            split,
            last,
            last_history,
            open_at_start: Cell::new(std::env::var(DETAIL_ENV).ok().filter(|v| !v.is_empty())),
            stack,
            error,
            home,
            view,
            reclaim,
            actions,
            probed: Cell::new(false),
        }
    }

    pub fn apply(&self, frame: Result<Frame, ReadError>) {
        let frame = match frame {
            Ok(f) => f,
            Err(e) => {
                // A descricao do AdwStatusPage interpreta markup; o erro traz
                // caminho e texto do sistema, que podem ter `<` ou `&`.
                let text = format!("{e}. AgentCenter tries again every second.");
                self.error
                    .set_description(Some(&gtk::glib::markup_escape_text(&text)));
                self.stack.set_visible_child_name(PAGE_ERROR);
                return;
            }
        };
        // Selection notifications during model updates must see this frame,
        // never the previous PID incarnation.
        self.last.replace(Some(Arc::clone(&frame.snapshot)));
        self.last_history.replace(frame.history.clone());
        let snap = &frame.snapshot;
        let m = &snap.machine;
        self.banner
            .set_revealed(m.io_stall_source == IoStallSource::DState);
        self.header.apply(snap);
        self.dashboard.apply(snap);

        let mut model = self.model.borrow_mut();
        let applied = model.apply(snap, self.home.as_deref());
        self.list.update(&applied);
        let n = model.len();
        let subtitle = match n {
            0 => format!("No sessions · {} processes", m.processes),
            1 => format!(
                "1 session · {} of {} processes",
                m.session_processes, m.processes
            ),
            _ => format!(
                "{n} sessions · {} of {} processes",
                m.session_processes, m.processes
            ),
        };
        if self.title.subtitle() != subtitle {
            self.title.set_subtitle(&subtitle);
        }
        let page = if n == 0 && !self.list.has_filter() {
            PAGE_EMPTY
        } else {
            PAGE_LIST
        };
        if self.stack.visible_child_name().as_deref() != Some(page) {
            self.stack.set_visible_child_name(page);
        }
        drop(model);

        self.detail
            .apply(snap, frame.history.as_ref(), self.home.as_deref());
        if let Some(want) = self.open_at_start.take() {
            match pick_detail(&want, snap) {
                Some(pid) if self.list.select_pid(pid) => {
                    // Foco na linha antes do push, como depois de Enter ou
                    // clique: o AdwNavigationView guarda o foco da pagina no
                    // push e o devolve no pop.
                    self.list.focus_selected();
                    // Sem efeito com o split aberto; recolhido, empurra o detalhe.
                    self.split.set_show_content(true);
                    // Para o measure-gui.sh --detail dizer qual sessao mediu.
                    let procs = snap
                        .sessions
                        .iter()
                        .find(|s| s.pid == pid)
                        .map_or(0, |s| s.procs);
                    println!("detail_pid={pid} detail_procs={procs}");
                    if std::env::var_os(DETAIL_BACK_ENV).is_some_and(|v| v == "1") {
                        self.probe_back();
                    }
                }
                _ if n == 0 => self.open_at_start.set(Some(want)),
                _ => eprintln!("agentcenter: {DETAIL_ENV}={want}: no such session"),
            }
        }
        self.reclaim.observe(&frame.snapshot);
        if self.view.visible_child_name().as_deref() == Some(VIEW_RECLAIM) {
            self.reclaim.refresh(self.home.as_deref());
        }
        if !self.probed.replace(true) {
            self.actions.run_probe();
        }
        self.last.replace(Some(Arc::clone(&frame.snapshot)));
        self.last_history.replace(frame.history.clone());
    }
}

impl Window {
    /// Ver [`DETAIL_BACK_ENV`]. A volta devolve o foco a linha da sessao no
    /// idle seguinte (`connect_show_content_notify`); o relato sai depois.
    /// O pop so sai com o detalhe ja alocado: antes do primeiro layout o
    /// breakpoint ainda nao recolheu o split, e um pop ali nao e a volta do
    /// usuario (o gancho do foco nem arma).
    fn probe_back(&self) {
        let (win, page) = (self.window.clone(), self.detail.page.clone());
        let split = self.split.clone();
        let popped = Rc::new(Cell::new(false));
        if let Some(sidebar) = split.sidebar() {
            // Conectado depois do handler do foco: o idle daqui roda depois
            // do idle dele.
            let (win, split, popped) = (win.clone(), split.clone(), popped.clone());
            sidebar.connect_shown(move |_| {
                if !popped.get() {
                    return;
                }
                let (win, split) = (win.clone(), split.clone());
                gtk::glib::idle_add_local_once(move || {
                    let name = |w: Option<gtk::Widget>| {
                        w.map_or_else(|| "none".to_string(), |w| w.type_().name().to_string())
                    };
                    let focus = gtk::prelude::GtkWindowExt::focus(&win);
                    let forced = force_focus_ring(&win, focus.as_ref());
                    if !forced.is_empty() {
                        // A captura mostra estado forcado, nao o da sessao:
                        // quem le a saida precisa saber.
                        println!("back_forced={}", forced.join(","));
                    }
                    let parent = name(focus.as_ref().and_then(|f| f.parent()));
                    println!(
                        "back_focus={} in={parent} shows_content={}",
                        name(focus),
                        split.shows_content()
                    );
                });
            });
        }
        let tries = Cell::new(0u32);
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let ready = page.is_mapped()
                && page.height() > 0
                && (!split.is_collapsed() || split.shows_content());
            // 3 s sem layout: pop assim mesmo, e o relato diz o que achou.
            if !ready && tries.replace(tries.get() + 1) < 60 {
                return gtk::glib::ControlFlow::Continue;
            }
            // Sem a transicao: com a tela apagada ela atrasa o fim da volta e
            // deixa a janela sem no de render na hora da captura.
            if std::env::var_os("AGENTCENTER_SCREENSHOT").is_some()
                && let Some(settings) = gtk::Settings::default()
            {
                settings.set_gtk_enable_animations(false);
            }
            win.set_focus_visible(true);
            popped.set(true);
            if let Err(e) = page.activate_action("navigation.pop", None) {
                println!("back_focus=none error={e}");
            }
            gtk::glib::ControlFlow::Break
        });
    }
}

/// So para a sonda da volta. Com a tela apagada o compositor nao ativa a
/// janela, e a captura sai segundos depois da volta: o GtkWindow apaga o foco
/// visivel uns 3 s depois de ligado sem evento de teclado, e uma janela
/// inativa pode ficar em `:backdrop`. A captura entao nao mostraria o anel que
/// o teclado acende numa janela ativa. Com a janela inativa, segura os dois
/// estados ate o fim e devolve o que forcou, para a saida dizer.
fn force_focus_ring(
    win: &adw::ApplicationWindow,
    focus: Option<&gtk::Widget>,
) -> Vec<&'static str> {
    use gtk::StateFlags;
    let mut forced = Vec::new();
    if win.is_active() {
        return forced;
    }
    // Herdado: sai da janela e de todos os filhos. Volta se o GTK repuser.
    win.unset_state_flags(StateFlags::BACKDROP);
    win.connect_state_flags_changed(|w, _| {
        if w.state_flags().contains(StateFlags::BACKDROP) {
            w.unset_state_flags(StateFlags::BACKDROP);
        }
    });
    forced.push("active");
    // O GTK propaga o foco visivel da janela para a cadeia do foco.
    win.set_focus_visible(true);
    win.connect_focus_visible_notify(|w| {
        if !w.gets_focus_visible() {
            w.set_focus_visible(true);
        }
    });
    if let Some(focus) = focus
        && !focus.state_flags().contains(StateFlags::FOCUS_VISIBLE)
    {
        focus.set_state_flags(
            StateFlags::FOCUSED | StateFlags::FOCUS_VISIBLE | StateFlags::FOCUS_WITHIN,
            false,
        );
    }
    forced.push("focus_visible");
    forced
}

#[cfg(test)]
#[path = "window_qa_tests.rs"]
mod qa_tests;

/// Explica como ligar o delay accounting. A app nunca pede root: o comando e
/// para o terminal do usuario.
fn delayacct_dialog() -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(
        Some("Turn On Delay Accounting"),
        Some(
            "Delay accounting is off, so the STALL column counts threads waiting on \
             disk (D:n) instead of measuring how long each session waited. \
             AgentCenter never asks for root; run these in a terminal.",
        ),
    );
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 6);
    for (caption, cmd) in [
        ("Until the next reboot", ENABLE_NOW),
        ("On every boot", ENABLE_BOOT),
    ] {
        let c = gtk::Label::builder().label(caption).xalign(0.0).build();
        c.add_css_class("caption-heading");
        let code = gtk::Label::builder()
            .label(cmd)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .build();
        code.add_css_class("monospace");
        box_.append(&c);
        box_.append(&code);
    }
    dialog.set_extra_child(Some(&box_));
    dialog.add_response("close", "Close");
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog
}

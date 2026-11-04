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
use crate::detail::Detail;
use crate::machine_history::{MachineHistory, Readout};
use crate::machine_strip::MachineStrip;
use crate::map_page::{MapPage, Source};
use crate::prefs::{Prefs, WindowSize};
use crate::reclaim_page::ReclaimPage;
use crate::session_list::{SessionList, SessionModel, SortKey, scrolled};
use crate::spend::SpendTracker;
use crate::token_map::TokenInput;
use adw::prelude::*;
use agentcenter_core::procfs::ReadError;
use agentcenter_core::{Frame, History, IoStallSource, Snapshot};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

/// Width (sp) up to which the split is collapsed (07-gui.md: below 1000 the
/// map sits above the list and the detail is pushed over them).
pub const COLLAPSE_MAX_SP: f64 = 999.0;

/// Width (sp) up to which the strip is 2x2, the map is the one-dimensional
/// memory strip and the view switcher moves to the bottom bar.
pub const COMPACT_MAX_SP: f64 = 599.0;
/// File, View and Help show as header buttons from this width; narrower, one
/// primary menu holds them.
pub const HEADER_MENUS_MIN_SP: f64 = 1500.0;

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

const PAGE_LIST: &str = "list";
const PAGE_ERROR: &str = "error";

const ENABLE_NOW: &str = "sudo sysctl kernel.task_delayacct=1";
const ENABLE_BOOT: &str =
    "echo 'kernel.task_delayacct = 1' | sudo tee /etc/sysctl.d/90-task-delayacct.conf";

pub struct Window {
    pub window: adw::ApplicationWindow,
    title: adw::WindowTitle,
    banner: adw::Banner,
    strip: Rc<MachineStrip>,
    history: RefCell<MachineHistory>,
    /// The Reclaim page's window of ticks, kept here too so the map's
    /// Reclaim button counts what that page lists.
    reclaim_ring: RefCell<crate::reclaim_page::Ring>,
    map: Rc<MapPage>,
    #[cfg_attr(not(test), expect(dead_code, reason = "read by tests"))]
    switcher_bar: adw::ViewSwitcherBar,
    model: RefCell<SessionModel>,
    list: Rc<SessionList>,
    detail: Rc<Detail>,
    split: adw::NavigationSplitView,
    /// Under 1000 sp: the detail is a page pushed over the whole Sessions
    /// page; wider, it replaces the maps above the list.
    pub(crate) narrow: Rc<Cell<bool>>,
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
    /// One tracker for the token map and the list, fed on every snapshot.
    pub(crate) spend: Rc<RefCell<SpendTracker>>,
}

/// What the vertical paned's top child shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanedMode {
    Maps,
    Detail,
}

/// Rows the list keeps at the least, and a row's pitch at its tallest
/// density (40 px row, 1 px gap; the compact 34 + 4 then shows a sliver of
/// the sixth row, which says the list scrolls).
pub const LIST_MIN_ROWS: i32 = 5;
pub const ROW_PX: i32 = 41;
/// The list's own chrome above its rows: order bar, column header, padding.
const LIST_CHROME_PX: i32 = 115;

/// Height the list keeps below the maps: its chrome and [`LIST_MIN_ROWS`]
/// whole rows, the sixth a scroll away. The GTK test
/// `the_maps_take_most_of_the_window_and_the_list_shows_its_top_five` holds
/// these numbers to the real widgets.
pub const LIST_TOP_PX: i32 = LIST_CHROME_PX + LIST_MIN_ROWS * ROW_PX;

/// The maps' least height and the strips' fixed top (px).
const MAPS_MIN_PX: i32 = 220;
const STRIPS_TOP_PX: i32 = 64;

/// Top child height (px) of the paned for a content height `total`: the maps
/// everything above the list's top rows (owner, 25/10: squeezed maps were
/// unreadable), the detail 65 %, strips a fixed short line; a saved
/// (dragged) position replaces the default. The list always keeps
/// [`LIST_TOP_PX`], which wins over the top's minimum.
pub fn paned_position(total: i32, mode: PanedMode, strips: bool, saved: Option<i32>) -> i32 {
    let upper = (total - LIST_TOP_PX).max(0);
    let want = match (mode, strips) {
        (PanedMode::Maps, true) => return STRIPS_TOP_PX.min(upper),
        (PanedMode::Maps, false) => upper,
        (PanedMode::Detail, _) => total * 65 / 100,
    };
    saved.unwrap_or(want).max(MAPS_MIN_PX).min(upper)
}

/// Text scale factor (1.0 at the default 96 dpi): sp = px / scale.
fn text_scale() -> f64 {
    gtk::Settings::default()
        .map(|s| f64::from(s.gtk_xft_dpi()) / 1024.0 / 96.0)
        .filter(|f| *f >= 0.5)
        .unwrap_or(1.0)
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
        // File, View and Help from 1500 sp up; below that one primary menu
        // (F10) holds the same model. `HEADER_MENUS_MIN_SP` sets the switch.
        let menus = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        menus.set_valign(gtk::Align::Center);
        let divider = gtk::Separator::new(gtk::Orientation::Vertical);
        divider.set_size_request(1, 20);
        divider.set_valign(gtk::Align::Center);
        divider.set_margin_end(4);
        menus.append(&divider);
        for (label, model) in crate::menu::parts() {
            let button = gtk::MenuButton::builder()
                .label(label)
                .use_underline(true)
                .always_show_arrow(true)
                .menu_model(&model)
                .build();
            button.add_css_class("flat");
            menus.append(&button);
        }
        headerbar.pack_start(&menus);
        let primary_menu = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main Menu")
            .primary(true)
            .menu_model(&crate::menu::model())
            .visible(false)
            .build();
        headerbar.pack_end(&primary_menu);

        let banner = adw::Banner::builder()
            .title("I/O stall is estimated from threads waiting on disk")
            .button_label("Get Exact Stall")
            .revealed(false)
            .build();

        let strip = Rc::new(MachineStrip::new());
        let model = SessionModel::default();
        let list = Rc::new(SessionList::new(&model));
        list.set_sort_key(startup_sort_key(
            prefs,
            std::env::var_os("AGENTCENTER_SCREENSHOT").is_some(),
        ));
        let p = prefs.clone();
        list.connect_sort_changed(move |k| p.set_sort_key(k));
        let detail = Rc::new(Detail::new(live));
        let home = std::env::var("HOME").ok();

        let narrow = Rc::new(Cell::new(false));
        let map = Rc::new(MapPage::new());
        let content = gtk::Stack::new();
        content.add_named(map.widget(), Some("overview"));
        // Keep ONE NavigationPage as the split content. Nesting detail.page
        // inside another page disconnects navigation.pop and back-focus restore.
        detail.page.set_child(gtk::Widget::NONE);
        // Wide, the detail sits here in place of the maps; narrow, it moves
        // to `narrow_slot` inside the pushed page.
        let wide_detail = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.add_named(&wide_detail, Some("detail"));
        content.set_visible_child_name("overview");
        let reclaim_back = gtk::Button::with_label("Back to Reclaim");
        reclaim_back.set_visible(false);
        reclaim_back.set_halign(gtk::Align::Start);
        reclaim_back.set_size_request(44, 44);
        // Wide there is no navigation page to pop: the detail replaces the
        // maps above the list and this button (Esc, Alt+Left) brings them
        // back. Narrow, the page header's own back button does it.
        let map_back = gtk::Button::from_icon_name("go-previous-symbolic");
        map_back.set_tooltip_text(Some("Back to the maps"));
        map_back.set_visible(false);
        map_back.set_halign(gtk::Align::Start);
        map_back.set_size_request(44, 44);
        map_back.add_css_class("flat");
        detail.toolbar.set_vexpand(true);
        wide_detail.append(&map_back);
        wide_detail.append(&detail.toolbar);
        let narrow_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        narrow_slot.set_vexpand(true);
        let detail_body = gtk::Box::new(gtk::Orientation::Vertical, 8);
        detail_body.append(&reclaim_back);
        detail_body.append(&narrow_slot);
        detail.page.set_child(Some(&detail_body));
        content.set_vexpand(true);
        // Maps (or the detail) over the full-width list, one draggable edge.
        let paned = gtk::Paned::new(gtk::Orientation::Vertical);
        paned.set_start_child(Some(&content));
        paned.set_end_child(Some(&scrolled(&list)));
        paned.set_shrink_start_child(true);
        paned.set_shrink_end_child(true);
        paned.set_vexpand(true);
        let sidebar = adw::NavigationPage::builder()
            .title("Sessions")
            .child(&paned)
            .build();
        // Always collapsed: the detail is a pushed page when narrow, and never
        // shown beside the list.
        let split = adw::NavigationSplitView::builder()
            .sidebar(&sidebar)
            .content(&detail.page)
            .collapsed(true)
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
        body.append(&strip.widget);
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        body.append(&view);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&headerbar);
        let switcher_bar = adw::ViewSwitcherBar::builder().stack(&view).build();
        toolbar.add_bottom_bar(&switcher_bar);
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

        // Only the last matching breakpoint applies, so at the narrow width
        // the medium one is unapplied: `narrow` is on while either is.
        let relayout: Relayout = Rc::default();
        let width_flags: Rc<Cell<(bool, bool)>> = Rc::default();
        let flag = {
            let (flags, narrow, relayout) = (width_flags.clone(), narrow.clone(), relayout.clone());
            Rc::new(move |which: usize, on: bool| {
                let mut f = flags.get();
                if which == 0 {
                    f.0 = on;
                } else {
                    f.1 = on;
                }
                flags.set(f);
                let now = f.0 || f.1;
                if narrow.replace(now) != now {
                    let hook = relayout.borrow().clone();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
            })
        };
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            COLLAPSE_MAX_SP,
            adw::LengthUnit::Sp,
        ));
        let (st, f) = (strip.clone(), flag.clone());
        bp.connect_apply(move |_| {
            st.set_medium(true);
            f(0, true);
        });
        let (st, f) = (strip.clone(), flag.clone());
        bp.connect_unapply(move |_| {
            st.set_medium(false);
            f(0, false);
        });
        let collapse_menus = |bp: &adw::Breakpoint| {
            bp.add_setter(&menus, "visible", Some(&false.to_value()));
            bp.add_setter(&primary_menu, "visible", Some(&true.to_value()));
        };
        // Only the last matching breakpoint applies, so every narrower one
        // repeats the menu collapse.
        let bp_menus = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            HEADER_MENUS_MIN_SP - 1.0,
            adw::LengthUnit::Sp,
        ));
        collapse_menus(&bp_menus);
        window.add_breakpoint(bp_menus);
        collapse_menus(&bp);
        window.add_breakpoint(bp);
        let bp_narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            COMPACT_MAX_SP,
            adw::LengthUnit::Sp,
        ));
        collapse_menus(&bp_narrow);
        bp_narrow.add_setter(&switcher, "visible", Some(&false.to_value()));
        bp_narrow.add_setter(&switcher_bar, "reveal", Some(&true.to_value()));
        // The strip takes a flag, not a property: follow the breakpoint
        // through its signals. The maps region has its own breakpoints.
        let (st, f) = (strip.clone(), flag.clone());
        bp_narrow.connect_apply(move |_| {
            st.set_compact(true);
            f(1, true);
        });
        let (st, f) = (strip.clone(), flag);
        bp_narrow.connect_unapply(move |_| {
            st.set_compact(false);
            f(1, false);
        });
        window.add_breakpoint(bp_narrow);

        // The maps' strips turn on at 560 sp of window height or less.
        let (mp, last_h) = (map.clone(), Cell::new(0));
        window.add_tick_callback(move |w, _| {
            let h = w.height();
            if last_h.replace(h) != h {
                mp.set_height_sp(f64::from(h) / text_scale());
            }
            gtk::glib::ControlFlow::Continue
        });

        // Any dialog the window shows (confirmations, the delay-accounting
        // one) holds the list's reorder until it closes.
        let held = list.clone();
        window.connect_visible_dialog_notify(move |w| {
            held.set_dialog_open(w.visible_dialog().is_some())
        });

        let actions = Actions::new(&window, &toasts, live);
        actions.install();

        // Recolhido com o detalhe na frente: o cabecalho da pagina (voltar e
        // nome da sessao) no lugar do da janela, e sem a faixa da maquina, que
        // e da lista. Aberto: um cabecalho so, o da janela.
        let chrome = {
            let (toolbar, band, detail, split, narrow) = (
                toolbar.clone(),
                strip.widget.clone(),
                detail.clone(),
                split.clone(),
                narrow.clone(),
            );
            move || {
                let collapsed = narrow.get();
                let on_detail = collapsed && split.shows_content();
                detail.toolbar.set_reveal_top_bars(collapsed);
                toolbar.set_reveal_top_bars(!on_detail);
                toolbar.set_reveal_bottom_bars(!on_detail);
                band.set_visible(!on_detail);
            }
        };
        let chrome = Rc::new(chrome);
        chrome();
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
        // Back from the detail: focus the map tile that opened it, else the
        // session's row.
        let opener: Rc<Cell<Option<Source>>> = Rc::default();
        let selected_pid: Rc<Cell<Option<u32>>> = Rc::default();
        let restore_focus: Rc<dyn Fn()> = {
            let (list, map, opener, pid) = (
                list.clone(),
                map.clone(),
                opener.clone(),
                selected_pid.clone(),
            );
            Rc::new(move || {
                if let Some(source) = opener.take()
                    && map.focus_source(source, pid.get())
                {
                    return;
                }
                list.focus_selected();
            })
        };
        let focus_list: Rc<dyn Fn()> = {
            let (view, split, narrow, restore) = (
                view.clone(),
                split.clone(),
                narrow.clone(),
                restore_focus.clone(),
            );
            Rc::new(move || {
                if view.visible_child_name().as_deref() == Some(VIEW_SESSIONS)
                    && narrow.get()
                    && !split.shows_content()
                {
                    restore();
                }
            })
        };
        let (c, r, focus, nw) = (
            chrome.clone(),
            returning.clone(),
            focus_list.clone(),
            narrow.clone(),
        );
        split.connect_show_content_notify(move |split| {
            c();
            if nw.get() && !split.shows_content() {
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

        // Split open: back from the detail returns the content to the map and
        // focus to the session's row. The selection stays, so the row still
        // marks the session and the map keeps its mark; activating the row or
        // its tile again reopens the detail.
        let back_action = gtk::gio::SimpleAction::new("detail-back", None);
        back_action.set_enabled(false);
        let sync_back = {
            let (content, narrow, view, button, action) = (
                content.clone(),
                narrow.clone(),
                view.clone(),
                map_back.clone(),
                back_action.clone(),
            );
            Rc::new(move || {
                let on = !narrow.get()
                    && view.visible_child_name().as_deref() == Some(VIEW_SESSIONS)
                    && content.visible_child_name().as_deref() == Some("detail");
                button.set_visible(on);
                action.set_enabled(on);
            })
        };
        let s = sync_back.clone();
        content.connect_visible_child_name_notify(move |_| s());
        let s = sync_back.clone();
        view.connect_visible_child_name_notify(move |_| s());
        let go_back: Rc<dyn Fn()> = {
            let (content, restore) = (content.clone(), restore_focus.clone());
            Rc::new(move || {
                content.set_visible_child_name("overview");
                restore();
            })
        };
        let g = go_back.clone();
        back_action.connect_activate(move |_, _| g());
        let g = go_back;
        map_back.connect_clicked(move |_| g());
        gtk::prelude::ActionMapExt::add_action(&window, &back_action);
        // A disabled action leaves the key alone, so collapsed the page's own
        // Esc and Alt+Left (navigation.pop) still win.
        let keys = gtk::ShortcutController::new();
        for trigger in ["Escape", "<Alt>Left"] {
            keys.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::NamedAction::new("win.detail-back")),
            ));
        }
        window.add_controller(keys);
        // Mouse button 8 (back) leaves the detail like Esc: collapsed it pops
        // the page, wide it brings the maps back (the action is off otherwise).
        let mouse_back = gtk::GestureClick::builder().button(8).build();
        let (split_for_mouse, narrow_for_mouse, win_for_mouse) =
            (split.clone(), narrow.clone(), window.downgrade());
        mouse_back.connect_pressed(move |_, _, _, _| {
            if narrow_for_mouse.get() {
                split_for_mouse.set_show_content(false);
            } else if let Some(w) = win_for_mouse.upgrade() {
                gtk::prelude::ActionGroupExt::activate_action(&w, "detail-back", None);
            }
        });
        window.add_controller(mouse_back);

        // The paned's top edge: the default per mode, the user's drag kept
        // per mode (this run), always inside the clamps.
        let saved: Rc<Cell<(Option<i32>, Option<i32>)>> = Rc::default();
        let (dragging, guard) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
        let position: Rc<dyn Fn(Option<i32>)> = {
            let (paned, content, map, saved, guard) = (
                paned.clone(),
                content.clone(),
                map.clone(),
                saved.clone(),
                guard.clone(),
            );
            Rc::new(move |dragged| {
                let total = paned.height();
                if total <= 0 {
                    return;
                }
                let mode = if content.visible_child_name().as_deref() == Some("detail") {
                    PanedMode::Detail
                } else {
                    PanedMode::Maps
                };
                let mut kept = saved.get();
                let slot = match mode {
                    PanedMode::Maps => &mut kept.0,
                    PanedMode::Detail => &mut kept.1,
                };
                if dragged.is_some() {
                    *slot = dragged;
                }
                let at = paned_position(total, mode, map.is_compact(), *slot);
                saved.set(kept);
                if at != paned.position() {
                    guard.set(true);
                    paned.set_position(at);
                    guard.set(false);
                }
            })
        };
        let (pos, drag_on, g) = (position.clone(), dragging.clone(), guard);
        paned.connect_position_notify(move |p| {
            if !g.get() && drag_on.get() {
                pos(Some(p.position()));
            }
        });
        let pos = position.clone();
        paned.connect_max_position_notify(move |_| pos(None));
        let pos = position.clone();
        content.connect_visible_child_name_notify(move |_| pos(None));
        let pos = position.clone();
        map.connect_layout_changed(move || pos(None));
        // Only a drag that starts on the handle moves the saved edge.
        let drag = gtk::GestureDrag::new();
        drag.set_propagation_phase(gtk::PropagationPhase::Capture);
        let (p, on) = (paned.clone(), dragging.clone());
        drag.connect_drag_begin(move |_, _, y| {
            on.set((y - f64::from(p.position())).abs() < 12.0);
        });
        drag.connect_drag_end(move |_, _, _| dragging.set(false));
        paned.add_controller(drag);

        // Crossing 1000 sp moves the detail between the maps' slot and the
        // pushed page, and keeps what was open open.
        let relayout_fn: Rc<dyn Fn()> = {
            let (content, split, narrow, tb) = (
                content.clone(),
                split.clone(),
                narrow.clone(),
                detail.toolbar.clone(),
            );
            let (wide_detail, narrow_slot) = (wide_detail.clone(), narrow_slot.clone());
            let (chrome, sync_back, position) = (chrome.clone(), sync_back.clone(), position);
            Rc::new(move || {
                let n = narrow.get();
                let (from, to) = if n {
                    (&wide_detail, &narrow_slot)
                } else {
                    (&narrow_slot, &wide_detail)
                };
                if tb.parent().as_ref() == Some(from.upcast_ref::<gtk::Widget>()) {
                    from.remove(&tb);
                    to.append(&tb);
                }
                if n {
                    if content.visible_child_name().as_deref() == Some("detail") {
                        content.set_visible_child_name("overview");
                        split.set_show_content(true);
                    }
                } else if split.shows_content() {
                    content.set_visible_child_name("detail");
                    split.set_show_content(false);
                }
                chrome();
                sync_back();
                position(None);
            })
        };
        relayout.replace(Some(relayout_fn));

        let last: Rc<RefCell<Option<Arc<Snapshot>>>> = Rc::default();
        let last_history: Rc<RefCell<Option<Arc<History>>>> = Rc::default();
        let (d, lst, lh, h, mp) = (
            detail.clone(),
            last.clone(),
            last_history.clone(),
            home.clone(),
            map.clone(),
        );
        let content_for_open = content.clone();
        let split_for_end = split.clone();
        let (nw, sel_cell, content_sel) = (narrow.clone(), selected_pid.clone(), content.clone());
        list.connect_selected(move |pid| {
            mp.set_selected(pid);
            sel_cell.set(pid);
            // Narrow, with no session the pushed page would be blank: show
            // the list again. Wide, the detail takes the maps' place.
            if nw.get() {
                if pid.is_none() {
                    split_for_end.set_show_content(false);
                }
            } else {
                content_sel.set_visible_child_name(if pid.is_some() {
                    "detail"
                } else {
                    "overview"
                });
            }
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

        // A tile or a chip opens its session like a click on its row; the
        // Reclaim button is the Reclaim page.
        let (opened_list, opened_split, nw) = (list.clone(), split.clone(), narrow.clone());
        let opened_content = content_for_open.clone();
        let opener_map = opener.clone();
        map.connect_open_from(move |pid, source| {
            opener_map.set(Some(source));
            if opened_list.reveal_pid(pid) {
                if nw.get() {
                    opened_split.set_show_content(true);
                } else {
                    // Already selected, so no selection change reopens it.
                    opened_content.set_visible_child_name("detail");
                }
            }
        });
        let reclaim_view = view.clone();
        map.connect_reclaim(move || reclaim_view.set_visible_child_name(VIEW_RECLAIM));

        let (sp, nw, opener_row) = (split.clone(), narrow.clone(), opener.clone());
        list.connect_open(move |_| {
            opener_row.set(None);
            if nw.get() {
                sp.set_show_content(true);
            } else {
                content_for_open.set_visible_child_name("detail");
            }
        });

        // The Action column: select the session, show its detail as a click
        // on the row would, and press the detail's own control, so the
        // confirmation dialog is the one the detail shows. End is the list's
        // own `win.act`. A control the detail cannot run says so in a toast:
        // a click that lands on the detail with no dialog reads as broken.
        let (sp, nw, lst, d, content_act, toast) = (
            split.clone(),
            narrow.clone(),
            list.clone(),
            detail.clone(),
            content.clone(),
            toasts.clone(),
        );
        list.connect_row_action(move |pid, op| {
            use crate::actions_ui::RowOp;
            if !matches!(op, RowOp::Compact | RowOp::Handoff) || !lst.select_pid(pid) {
                return;
            }
            if nw.get() {
                sp.set_show_content(true);
            } else {
                content_act.set_visible_child_name("detail");
            }
            let toast = toast.clone();
            d.request(pid, op, move || {
                toast.add_toast(adw::Toast::new(&format!(
                    "{} is not available for this session now",
                    op.label()
                )));
            });
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
            strip,
            history: RefCell::new(MachineHistory::new()),
            reclaim_ring: RefCell::default(),
            map,
            switcher_bar,
            model: RefCell::new(model),
            list,
            detail,
            split,
            narrow,
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
            spend: Rc::default(),
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
        let readout = Readout::of(snap);
        {
            let mut history = self.history.borrow_mut();
            history.record(snap);
            self.strip.update(snap, &history);
        }
        {
            // One tracker for both views: updated first, then read by the
            // token map and the list.
            let mut spend = self.spend.borrow_mut();
            spend.update(snap.taken_at_ms, crate::spend::inputs(snap));
            self.map
                .set_token_input(TokenInput::from_snapshot(snap, &spend));
        }
        {
            let mut ring = self.reclaim_ring.borrow_mut();
            ring.push(&frame.snapshot);
            self.map
                .set_window_reclaim(ring.reclaim(Some(std::process::id())));
        }
        self.map.update(snap, &readout);
        self.list.describe(snap);

        let mut model = self.model.borrow_mut();
        let applied = model.apply(snap, self.home.as_deref());
        self.list.update(&applied);
        // After the model: a session new in this snapshot has its row now, so
        // it gets its spend at once instead of sorting last for a tick.
        self.list.set_spend(&self.spend.borrow());
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
        // No sessions is the map's own empty state, not a page of its own.
        if self.stack.visible_child_name().as_deref() != Some(PAGE_LIST) {
            self.stack.set_visible_child_name(PAGE_LIST);
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
                    // O split e sempre recolhido: set_show_content(true) empurra a
                    // pagina do detalhe, vazia na janela larga (la o detalhe mora
                    // no lugar dos mapas e a selecao ja o mostra). `narrow` so
                    // muda apos a primeira alocacao, entao a largura pedida decide:
                    // estreita empurra ja (sem animacao pendente na captura).
                    if (1..=COLLAPSE_MAX_SP as i32).contains(&self.window.default_width()) {
                        self.split.set_show_content(true);
                    }
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

/// The sort order the list starts in. A capture ignores the saved order, like
/// it ignores the saved colour scheme, so two captures of one fixture match.
fn startup_sort_key(prefs: &Prefs, capturing: bool) -> SortKey {
    if capturing {
        SortKey::default()
    } else {
        prefs.sort_key()
    }
}

impl Window {
    /// Ver [`DETAIL_BACK_ENV`]. Recolhido, a volta e o `navigation.pop` da
    /// pagina; com o split aberto, a acao `win.detail-back` (Esc, Alt+Esquerda).
    /// O pop so sai com o detalhe ja alocado: antes do primeiro layout o
    /// breakpoint ainda nao recolheu o split, e um pop ali nao e a volta do
    /// usuario (o gancho do foco nem arma); aberto, a acao so vale com o
    /// detalhe na frente, e antes disso ela volta sem fazer nada. O relato espera o foco assentar:
    /// ele e posto num idle, depois do `shown` ou do notify conforme a
    /// animacao, e aberto o split nem ha `shown`; um idle da sonda corria na
    /// frente dele e lia o foco antes da volta.
    fn probe_back(&self) {
        let (win, page) = (self.window.clone(), self.detail.page.clone());
        let (split, narrow) = (self.split.clone(), self.narrow.clone());
        let tries = Cell::new(0u32);
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            // Open, the detail is there once the selection reached the content
            // stack (an idle after the select): the action is enabled then.
            let wide_open = || {
                gtk::gio::prelude::ActionMapExt::lookup_action(&win, "detail-back")
                    .is_some_and(|a| a.is_enabled())
            };
            let ready = page.is_mapped()
                && page.height() > 0
                && if narrow.get() {
                    split.shows_content()
                } else {
                    wide_open()
                };
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
            let result = if narrow.get() {
                page.activate_action("navigation.pop", None)
            } else {
                gtk::prelude::WidgetExt::activate_action(&win, "win.detail-back", None)
            };
            if let Err(e) = result {
                println!("back_focus=none error={e}");
                return gtk::glib::ControlFlow::Break;
            }
            let (win, split) = (win.clone(), split.clone());
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
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
/// The window's re-layout hook, set once the page widgets exist.
type Relayout = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

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

//! Isolated GTK action-path test; no OS keyboard injection or live controls.
use super::*;

fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut out = vec![root.clone()];
    let mut child = root.first_child();
    while let Some(widget) = child {
        out.extend(descendants(&widget));
        child = widget.next_sibling();
    }
    out
}

fn settle() {
    let until = std::time::Instant::now() + std::time::Duration::from_millis(350);
    while std::time::Instant::now() < until {
        let context = gtk::glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Settles until `done` holds, up to ~3.5 s: under a loaded suite rows and
/// idles land a few frames late. True when it held.
fn settle_until(done: impl Fn() -> bool) -> bool {
    for _ in 0..10 {
        if done() {
            return true;
        }
        settle();
    }
    done()
}

#[test]
fn cc2_native_reclaim_same_detail_back_and_async_refresh() {
    reclaim_return_scenario(false);
}

#[test]
fn cc2_native_reclaim_explicit_back_restores_navigation_chrome() {
    reclaim_return_scenario(true);
}

fn reclaim_return_scenario(explicit_back: bool) {
    gtk::test_synced(move || {
        adw::init().expect("isolated Broadway display required");
        let app = adw::Application::builder()
            .application_id(if explicit_back {
                "io.agentcenter.CC2ReclaimExplicitQA"
            } else {
                "io.agentcenter.CC2ReclaimPopQA"
            })
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let selected = Rc::new(Cell::new(None));
        let observed = selected.clone();
        let window = Rc::new(Window::new(
            &app,
            false,
            &Prefs::from_source(None, None),
            move |pid| observed.set(pid),
        ));
        window.window.set_default_size(360, 640);
        let mut fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/snapshots/uma.json")).unwrap();
        fixture["sessions"][0]["cpu_pct"] = serde_json::json!(0.0);
        fixture["sessions"][0]["overlay"]["status"] = serde_json::json!("idle");
        let snapshot: Snapshot = serde_json::from_value(fixture).unwrap();
        let pid = snapshot.sessions[0].pid;
        for tick in 0..crate::reclaim_page::WINDOW_TICKS {
            let mut sample = snapshot.clone();
            sample.taken_at_ms += tick as u64 * 1000;
            window.apply(Ok(Frame {
                snapshot: Arc::new(sample),
                history: None,
            }));
        }
        window.window.present();
        window.view.set_visible_child_name(VIEW_RECLAIM);
        window.reclaim.refresh(None);
        settle();
        let origin = descendants(window.reclaim.widget.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.tooltip_text().as_deref() == Some("View session"))
            .expect("idle fixture offers a session link");
        assert!(origin.grab_focus());
        let scroll = window.reclaim.scroll.vadjustment().value();
        let same_detail = window.detail.page.clone();
        assert!(origin.activate());
        settle();
        assert_eq!(
            window.view.visible_child_name().as_deref(),
            Some(VIEW_SESSIONS)
        );
        assert!(window.split.shows_content());
        assert_eq!(window.split.content(), Some(same_detail.clone()));
        assert_eq!(selected.get(), Some(pid));
        let back = descendants(window.window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Back to Reclaim"))
            .unwrap();
        assert!(back.is_visible());
        // Deliver a later sampler frame on the main loop, without a live sampler
        // or terminal owner. Selection and current focus must survive metrics.
        assert!(back.grab_focus());
        let later = window.clone();
        let mut updated = snapshot.clone();
        updated.taken_at_ms += 20_000;
        updated.sessions[0].pss_bytes = updated.sessions[0].pss_bytes.map(|bytes| bytes + 1024);
        gtk::glib::idle_add_local_once(move || {
            later.apply(Ok(Frame {
                snapshot: Arc::new(updated),
                history: None,
            }));
        });
        settle();
        assert_eq!(selected.get(), Some(pid));
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&window.window),
            Some(back.clone().upcast())
        );
        assert_eq!(window.split.content(), Some(same_detail));
        if explicit_back {
            assert!(back.activate());
        } else {
            window
                .detail
                .page
                .activate_action("navigation.pop", None)
                .unwrap();
        }
        settle();
        assert!(
            window.strip.widget.is_visible(),
            "Reclaim navigation chrome must return"
        );
        assert_eq!(
            window.view.visible_child_name().as_deref(),
            Some(VIEW_RECLAIM)
        );
        assert!(!window.split.shows_content());
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&window.window),
            Some(origin.upcast())
        );
        assert_eq!(window.reclaim.scroll.vadjustment().value(), scroll);
        window.window.close();
        settle();
    });
}

#[test]
fn cc2_native_disabled_focus_explains_itself_and_collapsed_back_restores_row_focus() {
    gtk::test_synced(|| {
        adw::init().expect("isolated Broadway display required");
        let app = adw::Application::builder()
            .application_id("io.agentcenter.CC2IsolatedQA")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let window = Window::new(&app, false, &Prefs::from_source(None, None), |_| {});
        window.window.set_default_size(360, 640);
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("../tests/fixtures/snapshots/uma.json")).unwrap();
        let pid = snapshot.sessions[0].pid;
        window.apply(Ok(Frame {
            snapshot: Arc::new(snapshot.clone()),
            history: None,
        }));
        window.window.present();
        settle();
        assert!(!window.split.shows_content());
        assert!(window.list.select_pid(pid));
        let list_view = descendants(window.window.upcast_ref())
            .into_iter()
            .filter(|w| w.has_css_class("session-rack"))
            .find_map(|w| w.downcast::<gtk::ListView>().ok())
            .unwrap();
        list_view.emit_by_name::<()>("activate", &[&0_u32]);
        settle();
        assert!(window.split.shows_content());
        let focus = descendants(window.window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Focus"))
            .expect("the detail's focus action (the title row shortens it to Focus at 360 sp)");
        assert!(
            !focus.is_sensitive(),
            "live=false cannot focus a real terminal"
        );
        assert!(
            descendants(window.window.upcast_ref()).iter().any(|w| {
                w.downcast_ref::<gtk::Label>()
                    .is_some_and(|l| l.label().contains("fixture"))
            }),
            "disabled explanation must be visible text"
        );
        window
            .detail
            .page
            .activate_action("navigation.pop", None)
            .unwrap();
        settle();
        assert!(!window.split.shows_content());
        let restored = gtk::prelude::GtkWindowExt::focus(&window.window).unwrap();
        let list_view = descendants(window.window.upcast_ref())
            .into_iter()
            .find(|w| w.has_css_class("session-rack"))
            .unwrap();
        assert!(restored.is_ancestor(&list_view) || restored == list_view);
        window.window.close();
        settle();
    });
}

fn tipico() -> Snapshot {
    serde_json::from_str(include_str!("../tests/fixtures/snapshots/tipico-40.json")).unwrap()
}

/// A window of `width` x `height` that has taken `snapshot` and settled.
fn open(id: &str, width: i32, height: i32, snapshot: &Snapshot) -> Window {
    adw::init().expect("isolated Broadway display required");
    let app = adw::Application::builder()
        .application_id(id)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let window = Window::new(&app, false, &Prefs::from_source(None, None), |_| {});
    window.window.set_default_size(width, height);
    window.apply(Ok(Frame {
        snapshot: Arc::new(snapshot.clone()),
        history: None,
    }));
    window.window.present();
    settle();
    window
}

#[test]
fn wide_window_has_strip_above_the_maps_with_the_list_below() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.WinWideQA", 1280, 800, &tipico());
        assert!(!window.narrow.get());
        assert!(window.strip.widget.is_visible());
        assert!(!window.strip.is_compact());
        assert!(!window.map.is_compact());
        assert!(!window.switcher_bar.reveals());
        assert_eq!(window.history.borrow().len(), 1, "a sample per frame");
        let sidebar = window.split.sidebar().unwrap();
        assert!(
            window.map.widget().is_ancestor(&sidebar),
            "the maps sit above the list in the main page"
        );
        window.window.close();
        settle();
    });
}

#[test]
fn mid_window_collapses_and_stacks_the_map_above_the_list() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.WinMidQA", 840, 700, &tipico());
        assert!(window.narrow.get());
        assert!(!window.strip.is_compact());
        assert!(!window.map.is_compact());
        assert!(!window.switcher_bar.reveals());
        let sidebar = window.split.sidebar().unwrap();
        assert!(window.map.widget().is_ancestor(&sidebar));
        window.window.close();
        settle();
    });
}

#[test]
fn narrow_window_compacts_strip_and_map_and_moves_the_switcher_down() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.WinNarrowQA", 360, 640, &tipico());
        assert!(window.narrow.get());
        assert!(window.strip.is_compact());
        assert!(window.map.is_compact());
        assert!(window.switcher_bar.reveals());
        window.window.close();
        settle();
    });
}

#[test]
#[ignore = "Broadway with no browser never applies a resize of a mapped window; run on the WSLg display"]
fn growing_back_undoes_the_compact_layout() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.WinGrowQA", 360, 640, &tipico());
        window.window.set_default_size(1100, 700);
        settle();
        assert!(!window.narrow.get());
        assert!(!window.strip.is_compact());
        assert!(!window.map.is_compact());
        assert!(!window.switcher_bar.reveals());
        assert_eq!(window.map.tier(), crate::map_page::Tier::Wide);
        window.window.close();
        settle();
    });
}

#[test]
fn a_waiting_chip_opens_its_session_and_reclaim_opens_the_page() {
    gtk::test_synced(|| {
        let snap = tipico();
        let window = open("io.agentcenter.WinChipQA", 1280, 800, &snap);
        let chip = descendants(window.window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.has_css_class("chip"))
            .expect("a waiting chip");
        chip.emit_clicked();
        settle();
        assert!(window.list.has_selection(), "the chip selected its session");
        let reclaim = descendants(window.window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.has_css_class("reclaim"))
            .expect("the Reclaim button");
        reclaim.emit_clicked();
        settle();
        assert_eq!(
            window.view.visible_child_name().as_deref(),
            Some(VIEW_RECLAIM)
        );
        window.window.close();
        settle();
    });
}

fn busiest_pid(snapshot: &Snapshot) -> u32 {
    snapshot
        .sessions
        .iter()
        .max_by_key(|s| s.procs)
        .unwrap()
        .pid
}

fn content_stack(window: &Window) -> gtk::Stack {
    descendants(window.window.upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Stack>().ok())
        .find(|s| s.child_by_name("overview").is_some() && s.child_by_name("detail").is_some())
        .expect("the content stack")
}

/// Focus is on a row of the session list, not on the window or the sort menu.
fn focus_is_on_a_list_row(window: &Window) -> bool {
    gtk::prelude::GtkWindowExt::focus(&window.window)
        .is_some_and(|f| f.type_().name() == "GtkListItemWidget" || f.is::<gtk::ListView>())
}

#[test]
fn wide_back_returns_the_content_to_the_map_and_focuses_the_row() {
    gtk::test_synced(|| {
        let snap = tipico();
        let window = open("io.agentcenter.WinBackWideQA", 1280, 800, &snap);
        assert!(!window.narrow.get());
        let content = content_stack(&window);
        assert!(window.list.select_pid(busiest_pid(&snap)));
        settle();
        assert_eq!(content.visible_child_name().as_deref(), Some("detail"));
        let back = descendants(window.window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.tooltip_text().as_deref() == Some("Back to the maps"))
            .expect("a back button beside the session name");
        assert!(back.is_mapped(), "the button shows with the detail open");
        back.emit_clicked();
        settle();
        assert_eq!(content.visible_child_name().as_deref(), Some("overview"));
        assert!(!back.is_mapped(), "nothing to go back from on the map");
        assert!(focus_is_on_a_list_row(&window), "focus returns to the row");
        window.window.close();
        settle();
    });
}

#[test]
fn wide_escape_action_goes_back_only_while_the_detail_is_open() {
    gtk::test_synced(|| {
        let snap = tipico();
        let window = open("io.agentcenter.WinBackEscQA", 1280, 800, &snap);
        let content = content_stack(&window);
        assert!(window.list.select_pid(busiest_pid(&snap)));
        settle();
        gtk::prelude::WidgetExt::activate_action(&window.window, "win.detail-back", None)
            .expect("the back action of Esc and Alt+Left");
        settle();
        assert_eq!(content.visible_child_name().as_deref(), Some("overview"));
        assert!(focus_is_on_a_list_row(&window));
        let action = gtk::gio::prelude::ActionMapExt::lookup_action(&window.window, "detail-back")
            .and_downcast::<gtk::gio::SimpleAction>()
            .unwrap();
        assert!(!action.is_enabled(), "on the map Esc is left to others");
        window.window.close();
        settle();
    });
}

#[test]
fn wide_clicking_the_same_chip_again_reopens_the_detail() {
    gtk::test_synced(|| {
        let snap = tipico();
        let window = open("io.agentcenter.WinBackAgainQA", 1280, 800, &snap);
        let content = content_stack(&window);
        let chip = descendants(window.window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.has_css_class("chip"))
            .expect("a waiting chip");
        chip.emit_clicked();
        settle();
        assert_eq!(content.visible_child_name().as_deref(), Some("detail"));
        gtk::prelude::WidgetExt::activate_action(&window.window, "win.detail-back", None).unwrap();
        settle();
        assert_eq!(content.visible_child_name().as_deref(), Some("overview"));
        chip.emit_clicked();
        settle();
        assert_eq!(content.visible_child_name().as_deref(), Some("detail"));
        window.window.close();
        settle();
    });
}

#[test]
fn collapsed_back_returns_focus_to_the_session_row() {
    gtk::test_synced(|| {
        let snap = tipico();
        let window = open("io.agentcenter.WinBackNarrowQA", 840, 700, &snap);
        assert!(window.narrow.get());
        assert!(window.list.select_pid(busiest_pid(&snap)));
        window.list.focus_selected();
        window.split.set_show_content(true);
        settle();
        assert!(window.split.shows_content());
        window
            .detail
            .page
            .activate_action("navigation.pop", None)
            .unwrap();
        settle();
        assert!(!window.split.shows_content());
        assert!(focus_is_on_a_list_row(&window));
        window.window.close();
        settle();
    });
}

#[test]
fn a_capture_ignores_the_saved_sort_order() {
    gtk::test_synced(|| {
        let dir = crate::prefs::test_support::compiled("window-sort");
        let prefs = crate::prefs::test_support::memory_prefs(&dir);
        prefs.set_sort_key(SortKey::Memory);
        assert_eq!(startup_sort_key(&prefs, false), SortKey::Memory);
        assert_eq!(startup_sort_key(&prefs, true), SortKey::default());
    });
}

fn without_session(snapshot: &Snapshot, pid: u32) -> Snapshot {
    let mut next = snapshot.clone();
    next.taken_at_ms += 1000;
    next.sessions.retain(|s| s.pid != pid);
    next
}

#[test]
fn collapsed_detail_closes_when_its_session_ends() {
    gtk::test_synced(|| {
        let snap = tipico();
        let window = open("io.agentcenter.WinEndedNarrowQA", 840, 700, &snap);
        assert!(window.narrow.get());
        let pid = busiest_pid(&snap);
        assert!(window.list.select_pid(pid));
        window.split.set_show_content(true);
        settle();
        assert!(window.split.shows_content());
        window.apply(Ok(Frame {
            snapshot: Arc::new(without_session(&snap, pid)),
            history: None,
        }));
        settle();
        assert!(!window.list.has_selection(), "the session is gone");
        assert!(
            !window.split.shows_content(),
            "no blank page under the back header: the list shows again"
        );
        window.window.close();
        settle();
    });
}

#[test]
fn wide_detail_back_is_off_on_the_reclaim_view() {
    gtk::test_synced(|| {
        let snap = tipico();
        let window = open("io.agentcenter.WinBackReclaimQA", 1280, 800, &snap);
        assert!(window.list.select_pid(busiest_pid(&snap)));
        settle();
        let action = gtk::gio::prelude::ActionMapExt::lookup_action(&window.window, "detail-back")
            .and_downcast::<gtk::gio::SimpleAction>()
            .unwrap();
        assert!(action.is_enabled(), "detail open on the Sessions view");
        window.view.set_visible_child_name(VIEW_RECLAIM);
        settle();
        assert!(
            !action.is_enabled(),
            "Esc must not flip a stack the user cannot see"
        );
        window.view.set_visible_child_name(VIEW_SESSIONS);
        settle();
        assert!(action.is_enabled(), "back on Sessions, the detail is open");
        window.window.close();
        settle();
    });
}

#[test]
fn escape_and_alt_left_are_bound_to_the_back_action() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.WinKeysQA", 1280, 800, &tipico());
        let controllers = window.window.observe_controllers();
        let mut triggers = Vec::new();
        for i in 0..controllers.n_items() {
            let Some(keys) = controllers
                .item(i)
                .and_downcast::<gtk::ShortcutController>()
            else {
                continue;
            };
            for j in 0..keys.n_items() {
                let shortcut = keys.item(j).and_downcast::<gtk::Shortcut>().unwrap();
                let named = shortcut
                    .action()
                    .and_downcast::<gtk::NamedAction>()
                    .map(|a| a.action_name().to_string());
                if named.as_deref() == Some("win.detail-back") {
                    triggers.push(shortcut.trigger().unwrap().to_str().to_string());
                }
            }
        }
        triggers.sort();
        assert_eq!(triggers, ["<Alt>Left", "Escape"]);
        window.window.close();
        settle();
    });
}

// ---- Layout card (dbce1c48): maps on top, full-width list below -------------

fn bounds_in(w: &impl IsA<gtk::Widget>, root: &impl IsA<gtk::Widget>) -> gtk::graphene::Rect {
    w.compute_bounds(root).expect("allocated and mapped")
}

fn mapped_tokens_and_memory(window: &Window) -> (bool, bool) {
    (
        window.map.token_map().is_mapped(),
        window.map.memory_map().is_mapped(),
    )
}

#[test]
fn paned_position_gives_the_maps_all_but_the_top_five_list_rows() {
    let list = LIST_TOP_PX;
    // Maps: everything above the list's bar, header and five rows.
    assert_eq!(
        paned_position(800, PanedMode::Maps, false, None),
        800 - list
    );
    assert_eq!(
        paned_position(1200, PanedMode::Maps, false, None),
        1200 - list
    );
    // Too short for 220 px of maps and five rows: the rows win.
    assert_eq!(
        paned_position(400, PanedMode::Maps, false, None),
        400 - list
    );
    // Detail: 65 %, still leaving the rows.
    assert_eq!(paned_position(1000, PanedMode::Detail, false, None), 650);
    assert_eq!(
        paned_position(500, PanedMode::Detail, false, None),
        500 - list
    );
    // Strips replace the maps: a fixed short top.
    assert_eq!(paned_position(700, PanedMode::Maps, true, None), 64);
    // A dragged position is kept, between 220 px of maps and five rows.
    assert_eq!(paned_position(1000, PanedMode::Maps, false, Some(300)), 300);
    assert_eq!(
        paned_position(1000, PanedMode::Maps, false, Some(900)),
        1000 - list
    );
    assert_eq!(paned_position(1000, PanedMode::Maps, false, Some(10)), 220);
}

/// Owner (25/10): the maps were squeezed and unreadable. The maps take the
/// larger share and the list shows its top five sessions, the rest a scroll
/// away.
#[test]
fn the_maps_take_most_of_the_window_and_the_list_shows_its_top_five() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.LayoutTopFiveQA", 1280, 800, &tipico());
        let root = &window.window;
        let all = descendants(root.upcast_ref());
        let paned = all
            .iter()
            .find_map(|w| w.clone().downcast::<gtk::Paned>().ok())
            .expect("the maps/list paned");
        let rack = all
            .iter()
            .find(|w| w.has_css_class("session-rack"))
            .expect("the list view")
            .clone();
        let viewport = rack
            .ancestor(gtk::ScrolledWindow::static_type())
            .expect("the list scrolls");
        // Under a loaded suite the paned and the list view settle a few
        // frames late: read until the layout holds still, up to ~3.5 s.
        let measure = || {
            let v = bounds_in(&viewport, root);
            let rows: Vec<_> = descendants(&rack)
                .into_iter()
                .filter(|w| w.has_css_class("session-row") && w.is_drawable())
                .map(|w| bounds_in(&w, root))
                .collect();
            let whole = rows
                .iter()
                .filter(|r| r.y() >= v.y() - 1.0 && r.y() + r.height() <= v.y() + v.height() + 1.0)
                .count();
            (
                paned.position(),
                paned.height() - paned.position(),
                rows.len(),
                whole,
            )
        };
        // The top five: the list read from its top, wherever the selection
        // scrolled it.
        let top = || {
            viewport
                .downcast_ref::<gtk::ScrolledWindow>()
                .expect("scrolled window")
                .vadjustment()
                .set_value(0.0)
        };
        top();
        settle();
        let mut seen = measure();
        for _ in 0..10 {
            if seen.0 > seen.1 && seen.3 == 5 {
                break;
            }
            top();
            settle();
            seen = measure();
        }
        let (maps, list, rows, whole) = seen;
        assert!(maps > list, "maps {maps} px over list {list} px");
        assert!(
            rows >= 5 && window.model.borrow().len() > 5,
            "more sessions than rows in view"
        );
        assert_eq!(whole, 5, "five whole rows in view, the sixth scrolled");
        window.window.close();
        settle();
    });
}

#[test]
fn wide_window_puts_the_token_map_left_and_the_memory_map_right() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.LayoutWideQA", 1280, 800, &tipico());
        assert_eq!(window.map.tier(), crate::map_page::Tier::Wide);
        assert_eq!(mapped_tokens_and_memory(&window), (true, true));
        let root = &window.window;
        let (t, m) = (
            bounds_in(window.map.token_map(), root),
            bounds_in(window.map.memory_map(), root),
        );
        assert!(t.x() + t.width() <= m.x() + 1.0, "tokens left of memory");
        assert!((t.width() - m.width()).abs() <= 4.0, "50/50 halves");
        window.window.close();
        settle();
    });
}

#[test]
fn the_list_spans_the_full_width_below_the_maps_and_keeps_five_rows() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.LayoutListQA", 1280, 800, &tipico());
        let root = &window.window;
        let rack = descendants(root.upcast_ref())
            .into_iter()
            .find(|w| w.has_css_class("session-rack"))
            .expect("the list view");
        let (l, m) = (
            bounds_in(&rack, root),
            bounds_in(window.map.memory_map(), root),
        );
        assert!(l.y() >= m.y() + m.height() - 1.0, "list below the maps");
        assert!(
            l.width() >= 1280.0 * 0.95,
            "list is full width: {}",
            l.width()
        );
        assert!(
            l.height() >= (LIST_MIN_ROWS * ROW_PX) as f32,
            "five rows of room: {}",
            l.height()
        );
        window.window.close();
        settle();
    });
}

#[test]
fn mid_window_shows_one_map_with_a_tokens_memory_toggle() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.LayoutMidQA", 840, 700, &tipico());
        assert_eq!(window.map.tier(), crate::map_page::Tier::Toggle);
        assert!(window.map.toggle().is_mapped(), "the toggle shows");
        assert_eq!(window.map.shown_map(), crate::map_page::Shown::Tokens);
        assert_eq!(mapped_tokens_and_memory(&window), (true, false));
        window.map.toggle().set_active_name(Some("memory"));
        settle();
        assert_eq!(window.map.shown_map(), crate::map_page::Shown::Memory);
        assert_eq!(mapped_tokens_and_memory(&window), (false, true));
        window.map.toggle().set_active_name(Some("tokens"));
        settle();
        assert_eq!(mapped_tokens_and_memory(&window), (true, false));
        window.window.close();
        settle();
    });
}

#[test]
fn narrow_window_shows_two_labelled_strips_and_no_map() {
    gtk::test_synced(|| {
        // The cockpit fixture carries token burn; tipico-40 has none, and an
        // empty Tokens strip is left out at 360 px
        // (at_360_the_tokens_strip_is_left_out_while_it_has_no_sessions).
        let snap: Snapshot =
            serde_json::from_str(include_str!("../tests/fixtures/snapshots/cockpit.json")).unwrap();
        let window = open("io.agentcenter.LayoutNarrowQA", 360, 640, &snap);
        assert!(
            !window.map.token_input().sessions.is_empty(),
            "the fixture carries token spend"
        );
        assert_eq!(window.map.tier(), crate::map_page::Tier::Strips);
        assert_eq!(mapped_tokens_and_memory(&window), (false, false));
        let labels: Vec<String> = descendants(window.map.widget().upcast_ref())
            .into_iter()
            .filter(|w| w.is_mapped())
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .map(|l| l.label().to_string())
            .collect();
        for word in ["Tokens", "Memory"] {
            assert!(labels.iter().any(|l| l == word), "{word} strip label");
        }
        window.window.close();
        settle();
    });
}

#[test]
fn a_short_window_turns_the_maps_into_strips() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.LayoutShortQA", 1280, 800, &tipico());
        window.map.set_height_sp(560.0);
        settle_until(|| mapped_tokens_and_memory(&window) == (false, false));
        assert_eq!(mapped_tokens_and_memory(&window), (false, false));
        window.map.set_height_sp(900.0);
        settle_until(|| mapped_tokens_and_memory(&window) == (true, true));
        assert_eq!(mapped_tokens_and_memory(&window), (true, true));
        window.window.close();
        settle();
    });
}

/// The strips key on height as well as width: a short, wide window gets
/// them, and with them the compact `strips` class.
#[test]
fn a_short_wide_window_gets_the_strips_class() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.ShortWideStripsQA", 1280, 800, &tipico());
        settle();
        assert!(!window.map.widget().has_css_class("strips"));
        assert!(resize_until(&window, 500, (false, false)));
        assert!(
            window.map.widget().has_css_class("strips"),
            "a 1280 x 500 window shows the strips without the strips class"
        );
        window.window.close();
        settle();
    });
}

/// Resizes the open window to `height` and settles until the maps show
/// `want` (tokens, memory), up to ~3.5 s.
fn resize_until(window: &Window, height: i32, want: (bool, bool)) -> bool {
    window.window.set_default_size(1280, height);
    settle_until(|| mapped_tokens_and_memory(window) == want)
}

#[test]
fn the_maps_follow_the_window_height_across_the_560_sp_threshold() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.LayoutHeightFollowQA", 1280, 800, &tipico());
        assert_eq!(mapped_tokens_and_memory(&window), (true, true));
        assert!(
            resize_until(&window, 500, (false, false)),
            "a window 500 px tall turns the maps into strips"
        );
        assert!(
            resize_until(&window, 800, (true, true)),
            "a window 800 px tall brings the maps back"
        );
        window.window.close();
        settle();
    });
}

/// The height reaches the maps from a signal, not from a callback run on
/// every frame: that callback kept the GPU clock awake while idle (3.35 %
/// of a core against a 2 % budget).
#[test]
fn the_height_check_registers_no_per_frame_tick_callback() {
    let source = include_str!("window.rs");
    let lines: Vec<&str> = source.lines().collect();
    let sets: Vec<usize> = (0..lines.len())
        .filter(|&i| {
            lines[i].contains("set_height_sp(") && !lines[i].trim_start().starts_with("//")
        })
        .collect();
    assert!(!sets.is_empty(), "window.rs still feeds the maps a height");
    for i in sets {
        let near = &lines[i.saturating_sub(8)..=i];
        assert!(
            !near.iter().any(|l| l.contains("add_tick_callback")),
            "line {}: the height is fed from a per-frame tick callback",
            i + 1
        );
    }
}

#[test]
fn chips_verdict_and_reclaim_share_one_row_spanning_both_maps() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.LayoutRowQA", 1280, 800, &tipico());
        let root = &window.window;
        let (chips, reclaim) = (window.map.chips_row(), window.map.reclaim_button());
        let (c, r) = (bounds_in(chips, root), bounds_in(reclaim, root));
        let row = bounds_in(window.map.status_row(), root);
        let (t, m) = (
            bounds_in(window.map.token_map(), root),
            bounds_in(window.map.memory_map(), root),
        );
        assert!(
            r.y() < t.y() && c.y() < t.y(),
            "the row sits above both maps"
        );
        assert!((c.y() - r.y()).abs() < 40.0, "chips and Reclaim on one row");
        assert!(row.x() <= t.x() + 1.0, "row starts over the token map");
        assert!(
            row.x() + row.width() >= m.x() + m.width() - 1.0,
            "row ends over the memory map"
        );
        assert!(
            r.x() + r.width() >= m.x() + m.width() - 24.0,
            "Reclaim sits at the right end"
        );
        window.window.close();
        settle();
    });
}

#[test]
fn memory_column_has_an_eight_px_capacity_bar_under_its_heading() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.LayoutCapQA", 1280, 800, &tipico());
        let root = &window.window;
        let bar = window.map.capacity_bar();
        assert!(bar.is_mapped(), "shown in the two-map layout");
        let (b, m) = (
            bounds_in(bar, root),
            bounds_in(window.map.memory_map(), root),
        );
        assert_eq!(b.height().round() as i32, 8);
        assert!(b.y() + b.height() <= m.y() + 1.0, "bar above the map");
        window.window.close();
        settle();
    });
}

#[test]
fn the_detail_replaces_the_maps_and_the_list_stays() {
    gtk::test_synced(|| {
        let snap = tipico();
        let window = open("io.agentcenter.LayoutDetailQA", 1280, 800, &snap);
        assert!(window.list.select_pid(busiest_pid(&snap)));
        settle();
        assert!(!window.map.memory_map().is_mapped(), "the maps give way");
        assert!(!window.map.token_map().is_mapped());
        assert!(window.detail.toolbar.is_mapped(), "the detail shows");
        let rack = descendants(window.window.upcast_ref())
            .into_iter()
            .find(|w| w.has_css_class("session-rack"))
            .unwrap();
        assert!(rack.is_mapped(), "the list stays below it");
        window.window.close();
        settle();
    });
}

#[test]
fn escape_returns_focus_to_the_memory_tile_that_opened_the_detail() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.LayoutEscTileQA", 1280, 800, &tipico());
        let map = window.map.memory_map();
        let tile = map
            .tiles()
            .into_iter()
            .find(|t| crate::memory_map::MemoryMap::tile_pid(t).is_some())
            .expect("a session tile");
        assert!(tile.grab_focus());
        let keys = map.observe_controllers();
        let ctl = (0..keys.n_items())
            .filter_map(|i| keys.item(i).and_downcast::<gtk::EventControllerKey>())
            .next()
            .unwrap();
        use gtk::glib::translate::IntoGlib;
        assert!(ctl.emit_by_name::<bool>(
            "key-pressed",
            &[
                &gtk::gdk::Key::Return.into_glib(),
                &0u32,
                &gtk::gdk::ModifierType::empty()
            ],
        ));
        settle();
        assert!(
            window.detail.toolbar.is_mapped(),
            "the tile opened the detail"
        );
        gtk::prelude::WidgetExt::activate_action(&window.window, "win.detail-back", None).unwrap();
        settle();
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&window.window),
            Some(tile),
            "Esc returns to the tile"
        );
        window.window.close();
        settle();
    });
}

#[test]
fn escape_returns_focus_to_the_row_that_opened_the_detail() {
    gtk::test_synced(|| {
        let snap = tipico();
        let window = open("io.agentcenter.LayoutEscRowQA", 1280, 800, &snap);
        assert!(window.list.select_pid(busiest_pid(&snap)));
        window.list.focus_selected();
        let rack = descendants(window.window.upcast_ref())
            .into_iter()
            .find_map(|w| {
                w.has_css_class("session-rack")
                    .then(|| w.downcast::<gtk::ListView>().ok())
                    .flatten()
            })
            .unwrap();
        rack.emit_by_name::<()>("activate", &[&0_u32]);
        settle();
        // Focus is somewhere else when Back runs (the list is always on screen,
        // so without this the row would keep it by itself).
        gtk::prelude::GtkWindowExt::set_focus(&window.window, gtk::Widget::NONE);
        assert!(!focus_is_on_a_list_row(&window));
        gtk::prelude::WidgetExt::activate_action(&window.window, "win.detail-back", None).unwrap();
        settle();
        assert!(focus_is_on_a_list_row(&window));
        window.window.close();
        settle();
    });
}

#[test]
fn every_snapshot_updates_the_spend_tracker_and_feeds_the_token_map() {
    gtk::test_synced(|| {
        let mut snap = tipico();
        let key = (snap.sessions[0].pid, snap.sessions[0].starttime);
        let set = |snap: &mut Snapshot, v: u64| {
            let mut value = serde_json::to_value(&*snap).unwrap();
            value["sessions"][0]["overlay"]["spend_30m"] = serde_json::json!(v);
            *snap = serde_json::from_value(value).unwrap();
        };
        set(&mut snap, 12_345);
        let window = open("io.agentcenter.LayoutSpendQA", 1280, 800, &snap);
        assert_eq!(window.spend.borrow().spend_30m(key), Some(12_345));
        assert_eq!(
            window.map.token_input(),
            crate::token_map::TokenInput::from_snapshot(&snap, &window.spend.borrow()),
            "the map got the input built from the tracker"
        );
        let mut next = snap.clone();
        next.taken_at_ms += 1000;
        set(&mut next, 99_000);
        window.apply(Ok(Frame {
            snapshot: Arc::new(next),
            history: None,
        }));
        assert_eq!(window.spend.borrow().spend_30m(key), Some(99_000));
        window.window.close();
        settle();
    });
}

// ---- Header card (68b4d6bd): File, View and Help as header buttons ----------

/// Every action name a menu model reaches, submenus and sections included.
fn model_actions(model: &gtk::gio::MenuModel, out: &mut Vec<String>) {
    for i in 0..model.n_items() {
        if let Some(action) = model
            .item_attribute_value(i, "action", Some(gtk::glib::VariantTy::STRING))
            .and_then(|v| v.get::<String>())
        {
            let target = model
                .item_attribute_value(i, "target", None)
                .map(|t| t.print(false).to_string())
                .unwrap_or_default();
            out.push(format!("{action}({target})"));
        }
        for link in ["submenu", "section"] {
            if let Some(inner) = model.item_link(i, link) {
                model_actions(&inner, out);
            }
        }
    }
}

fn actions_of(model: &gtk::gio::MenuModel) -> Vec<String> {
    let mut out = Vec::new();
    model_actions(model, &mut out);
    out.sort();
    out
}

fn menu_buttons(window: &Window) -> Vec<gtk::MenuButton> {
    descendants(window.window.upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::MenuButton>().ok())
        .collect()
}

/// The File/View/Help button of that name, mnemonic underscores ignored.
fn named_button(window: &Window, name: &str) -> Option<gtk::MenuButton> {
    menu_buttons(window)
        .into_iter()
        .find(|b| b.label().is_some_and(|l| l.replace('_', "") == name))
}

fn primary_button(window: &Window) -> Option<gtk::MenuButton> {
    menu_buttons(window)
        .into_iter()
        .find(|b| b.icon_name().as_deref() == Some("open-menu-symbolic"))
}

/// The window's own header bar: the one that holds the page switcher.
fn window_header(window: &Window) -> adw::HeaderBar {
    descendants(window.window.upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<adw::HeaderBar>().ok())
        .find(|b| {
            descendants(b.upcast_ref())
                .iter()
                .any(|w| w.is::<adw::InlineViewSwitcher>())
        })
        .expect("window header bar")
}

fn shown(w: &impl IsA<gtk::Widget>) -> bool {
    w.is_mapped() && w.is_child_visible()
}

fn old_menu_actions() -> Vec<String> {
    actions_of(crate::menu::model().upcast_ref())
}

fn no_menu_row(window: &Window) {
    assert!(
        !descendants(window.window.upcast_ref())
            .iter()
            .any(|w| w.is::<gtk::PopoverMenuBar>()),
        "the 30 px menu row is gone"
    );
}

#[test]
fn wide_header_has_file_view_help_buttons_and_no_menu_row() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.HdrWideQA", 1600, 800, &tipico());
        no_menu_row(&window);
        let mut reached = Vec::new();
        for name in ["File", "View", "Help"] {
            let button = named_button(&window, name).unwrap_or_else(|| panic!("{name} button"));
            assert!(shown(&button), "{name} is shown at 1600 px");
            assert!(button.has_css_class("flat"), "{name} is flat");
            assert!(
                button.property::<bool>("always-show-arrow"),
                "{name} has an arrow"
            );
            reached.extend(actions_of(&button.menu_model().expect("a menu model")));
        }
        reached.sort();
        assert_eq!(reached, old_menu_actions(), "no action lost");
        assert!(
            primary_button(&window).is_none_or(|b| !shown(&b)),
            "the primary menu is collapsed away"
        );
        window.window.close();
        settle();
    });
}

#[test]
fn header_buttons_sit_after_a_separator_next_to_the_switcher() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.HdrOrderQA", 1600, 800, &tipico());
        let file = named_button(&window, "File").expect("File button");
        let separator = descendants(window.window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Separator>().ok())
            .find(|s| {
                s.orientation() == gtk::Orientation::Vertical
                    && s.width_request() <= 1
                    && s.height_request() == 20
            })
            .expect("a 1x20 separator");
        let bar = window_header(&window);
        assert!(file.is_ancestor(&bar) && separator.is_ancestor(&bar));
        let at = |w: &gtk::Widget| w.compute_bounds(&bar).unwrap().x();
        assert!(at(separator.upcast_ref()) < at(file.upcast_ref()));
        assert!(
            at(window.switcher_bar.upcast_ref()) >= 0.0 && at(separator.upcast_ref()) > 0.0,
            "the separator follows the page switcher on the left"
        );
        window.window.close();
        settle();
    });
}

#[test]
fn below_1500_sp_one_primary_menu_holds_the_whole_model_and_answers_f10() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.HdrNarrowQA", 1280, 800, &tipico());
        no_menu_row(&window);
        let primary = primary_button(&window).expect("primary menu button");
        assert!(shown(&primary));
        assert!(
            primary.property::<bool>("primary"),
            "primary menus open on F10"
        );
        for name in ["File", "View", "Help"] {
            assert!(
                named_button(&window, name).is_none_or(|b| !shown(&b)),
                "{name} collapses into the primary menu below 1500 sp"
            );
        }
        assert_eq!(
            actions_of(&primary.menu_model().expect("a menu model")),
            old_menu_actions(),
            "the primary menu holds every action"
        );
        window.window.close();
        settle();
    });
}

#[test]
fn the_primary_menu_is_also_the_menu_at_phone_width() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.HdrPhoneQA", 360, 640, &tipico());
        no_menu_row(&window);
        let primary = primary_button(&window).expect("primary menu button");
        assert!(shown(&primary));
        assert!(named_button(&window, "File").is_none_or(|b| !shown(&b)));
        window.window.close();
        settle();
    });
}

#[test]
fn the_header_is_first_in_the_tab_order_and_holds_the_menu_buttons() {
    gtk::test_synced(|| {
        let window = open("io.agentcenter.HdrTabQA", 1600, 800, &tipico());
        let bar = window_header(&window);
        gtk::prelude::GtkWindowExt::set_focus(&window.window, gtk::Widget::NONE);
        let mut stops = Vec::new();
        for _ in 0..5 {
            assert!(window.window.child_focus(gtk::DirectionType::TabForward));
            stops.push(gtk::prelude::GtkWindowExt::focus(&window.window).unwrap());
        }
        let first_outside = stops.iter().position(|w| !w.is_ancestor(&bar));
        for name in ["File", "View", "Help"] {
            let button = named_button(&window, name).expect(name);
            let at = stops
                .iter()
                .position(|w| w == button.upcast_ref::<gtk::Widget>() || w.is_ancestor(&button))
                .unwrap_or_else(|| panic!("{name} is a Tab stop in the header"));
            assert!(
                first_outside.is_none_or(|o| at < o),
                "{name} comes before anything outside the header"
            );
        }
        assert!(
            stops[0].is_ancestor(&bar),
            "the first stop is in the header"
        );
        window.window.close();
        settle();
    });
}

/// Opens the detail of the busiest session in a window of `width` sp.
fn open_detail(id: &str, width: i32) -> (Window, gtk::Widget) {
    let snap = tipico();
    let window = open(id, width, 700, &snap);
    assert!(window.list.select_pid(busiest_pid(&snap)));
    let list_view = descendants(window.window.upcast_ref())
        .into_iter()
        .filter(|w| w.has_css_class("session-rack"))
        .find_map(|w| w.downcast::<gtk::ListView>().ok())
        .unwrap();
    list_view.emit_by_name::<()>("activate", &[&0_u32]);
    settle();
    let header = descendants(window.detail.toolbar.upcast_ref())
        .into_iter()
        .find(|w| w.is::<adw::HeaderBar>())
        .expect("the detail page's header");
    (window, header)
}

fn mapped_button(root: &gtk::Widget, label: Option<&str>, css: &str) -> Option<gtk::Button> {
    descendants(root)
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| {
            b.is_mapped()
                && (label.is_none() || b.label().as_deref() == label)
                && (css.is_empty() || b.has_css_class(css))
        })
}

fn detail_page_has_back_and_handoff_compact(id: &'static str, width: i32) {
    gtk::test_synced(move || {
        let (window, header) = open_detail(id, width);
        assert!(window.narrow.get());
        assert!(
            window.split.shows_content(),
            "the detail is a page over the list"
        );
        assert!(
            mapped_button(&header, None, "back").is_some(),
            "a visible back button in the page header at {width} sp"
        );
        for action in ["Handoff", "Compact"] {
            assert!(
                mapped_button(&header, Some(action), "").is_some(),
                "{action} sits in the page header at {width} sp"
            );
        }
        window.window.close();
        settle();
    });
}

#[test]
fn the_840_sp_detail_is_a_page_with_a_back_button_and_handoff_compact_in_its_header() {
    detail_page_has_back_and_handoff_compact("io.agentcenter.DetailPage840QA", 840);
}

#[test]
fn the_360_sp_detail_is_a_page_with_a_back_button_and_handoff_compact_in_its_header() {
    detail_page_has_back_and_handoff_compact("io.agentcenter.DetailPage360QA", 360);
}

/// The header buttons are a second door to the same controls, so they cannot
/// show once the detail shares the page with the maps (the wide window).
#[test]
fn wide_detail_has_no_page_header_and_returns_on_mouse_back_too() {
    gtk::test_synced(|| {
        let snap = tipico();
        let window = open("io.agentcenter.DetailWideMouseQA", 1280, 800, &snap);
        assert!(window.list.select_pid(busiest_pid(&snap)));
        settle();
        assert!(
            mapped_button(window.detail.toolbar.upcast_ref(), Some("Handoff"), "").is_none(),
            "no page header, so no header copy of Handoff"
        );
        let controllers = window.window.observe_controllers();
        let mouse_back = (0..controllers.n_items())
            .filter_map(|i| controllers.item(i))
            .filter_map(|c| c.downcast::<gtk::GestureClick>().ok())
            .find(|g| g.button() == 8)
            .expect("mouse button 8 (back) is bound on the window");
        let content = content_stack(&window);
        assert_eq!(content.visible_child_name().as_deref(), Some("detail"));
        mouse_back.emit_by_name::<()>("pressed", &[&1_i32, &0.0_f64, &0.0_f64]);
        settle();
        assert_eq!(content.visible_child_name().as_deref(), Some("overview"));
        window.window.close();
        settle();
    });
}

/// Owner's bug (25/10), end to end: with session X in the detail, the
/// Action button of row Y selects Y; the detail follows in an idle and
/// rediscovers the control. X's ready control is never pressed; Y's is,
/// once it is up.
#[test]
fn the_row_action_presses_the_control_once_its_session_is_shown() {
    gtk::test_synced(|| {
        let cockpit: Snapshot =
            serde_json::from_str(include_str!("../tests/fixtures/snapshots/cockpit.json")).unwrap();
        let window = open("io.agentcenter.RowActionQA", 1280, 800, &cockpit);
        let root = &window.window;
        let compact_row = || {
            descendants(root.upcast_ref())
                .into_iter()
                .filter(|w| w.has_css_class("row-action") && w.is_drawable())
                .filter_map(|w| w.downcast::<gtk::Button>().ok())
                .find(|b| b.tooltip_text().as_deref() == Some("Compact context"))
        };
        assert!(
            settle_until(|| compact_row().is_some()),
            "a row asking for Compact in view"
        );
        let action = compact_row().unwrap();
        // Learn Y's pid through its own Action, then show another session X.
        action.emit_clicked();
        assert!(
            settle_until(|| window.detail.shown_pid().is_some()),
            "Y shown"
        );
        let y = window.detail.shown_pid().unwrap();
        let store = window.model.borrow().store().clone();
        let x = (0..store.n_items())
            .filter_map(|i| {
                store
                    .item(i)
                    .and_downcast::<crate::session_object::SessionObject>()
            })
            .map(|o| o.pid())
            .find(|&p| p != y)
            .expect("a second session");
        assert!(window.list.select_pid(x));
        assert!(
            settle_until(|| window.detail.shown_pid() == Some(x)),
            "X in the detail"
        );
        let control = descendants(root.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Compact…"))
            .expect("the detail's Compact control");
        let clicks = Rc::new(Cell::new(0));
        let seen = clicks.clone();
        control.connect_clicked(move |_| seen.set(seen.get() + 1));
        // X's control is up when Y's Action is clicked.
        control.set_sensitive(true);
        action.emit_clicked();
        assert_eq!(clicks.get(), 0, "X's control is not Y's");
        assert!(
            settle_until(|| window.detail.shown_pid() == Some(y)),
            "Y in the detail"
        );
        assert_eq!(clicks.get(), 0, "Y's control is not up yet");
        // Y's capability read returns: its control comes up.
        control.set_sensitive(true);
        assert!(settle_until(|| clicks.get() > 0), "pressed for Y");
        settle();
        assert_eq!(clicks.get(), 1, "pressed once");
        window.window.close();
        settle();
    });
}

fn cockpit_with_subagents() -> Snapshot {
    let mut v: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/snapshots/cockpit.json")).unwrap();
    for (name, live, total) in [
        ("atlas-api-auth-refactor", 3, 5),
        ("quill-flaky-tests", 1, 2),
    ] {
        let s = v["sessions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|s| s["overlay"]["name"] == name)
            .unwrap();
        s["overlay"]["subagents_live"] = serde_json::json!(live);
        s["overlay"]["subagents_total"] = serde_json::json!(total);
    }
    serde_json::from_value(v).unwrap()
}

fn gauge(window: &Window) -> gtk::MenuButton {
    descendants(window.window.upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::MenuButton>().ok())
        .find(|b| b.has_css_class("session-gauge"))
        .expect("the session gauge")
}

/// (name, value) of each popover row, in order.
fn gauge_rows(button: &gtk::MenuButton) -> Vec<(String, String)> {
    let popover = button.popover().expect("the gauge popover");
    let text = |w: &gtk::Widget, class: &str| {
        descendants(w)
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .find(|l| l.has_css_class(class))
            .map(|l| l.label().to_string())
    };
    descendants(popover.upcast_ref())
        .into_iter()
        .filter(|w| w.has_css_class("gauge-row"))
        .map(|row| {
            (
                text(&row, "gauge-name").expect("row name"),
                text(&row, "gauge-value").expect("row value"),
            )
        })
        .collect()
}

fn gauge_number(button: &gtk::MenuButton) -> String {
    descendants(button.upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .find(|l| l.has_css_class("gauge-number"))
        .expect("the number inside the gauge")
        .label()
        .to_string()
}

fn row<'a>(rows: &'a [(String, String)], name: &str) -> &'a str {
    rows.iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no row {name}: {rows:?}"))
        .1
        .as_str()
}

#[test]
fn the_gauge_sits_in_the_machine_strip_and_counts_open_agent_sessions() {
    gtk::test_synced(|| {
        let window = open(
            "io.agentcenter.GaugeHdrQA",
            1280,
            800,
            &cockpit_with_subagents(),
        );
        let button = gauge(&window);
        assert!(
            button.is_ancestor(&window.strip.widget),
            "in the machine strip"
        );
        assert!(
            !button.is_ancestor(&window_header(&window)),
            "no longer in the header bar"
        );
        assert!(shown(&button));
        assert_eq!(gauge_number(&button), "7", "the herdr multiplexer is out");
        let label = "7 open sessions, 4 live sub-sessions; 11 of 40";
        assert_eq!(button.tooltip_text().as_deref(), Some(label));
        window.window.close();
        settle();
    });
}

/// The strip's own cell, drawn inside the strip and beside the other cells
/// in the row, and inside it in the 2x2 layout under 600 px.
fn gauge_cell_in_strip_at(id: &str, width: i32, height: i32) {
    let window = open(id, width, height, &cockpit_with_subagents());
    let button = gauge(&window);
    assert!(
        settle_until(|| button.is_mapped() && button.width() > 10),
        "the gauge is drawn"
    );
    let root = &window.window;
    let (b, strip) = (
        bounds_in(&button, root),
        bounds_in(&window.strip.widget, root),
    );
    assert!(
        b.x() >= strip.x()
            && b.x() + b.width() <= strip.x() + strip.width() + 0.5
            && b.y() >= strip.y()
            && b.y() + b.height() <= strip.y() + strip.height() + 0.5,
        "inside the strip at {width} px: {b:?} in {strip:?}"
    );
    window.window.close();
    settle();
}

#[test]
fn the_gauge_cell_fits_the_strip_in_a_row() {
    gtk::test_synced(|| gauge_cell_in_strip_at("io.agentcenter.GaugeRowQA", 1280, 800));
}

#[test]
fn the_gauge_cell_fits_the_strip_in_the_2x2_layout() {
    gtk::test_synced(|| {
        gauge_cell_in_strip_at("io.agentcenter.GaugeCompactQA", 360, 640);
    });
}

#[test]
fn the_popover_breaks_the_load_down_per_tool_and_per_session() {
    gtk::test_synced(|| {
        let window = open(
            "io.agentcenter.GaugePopQA",
            1280,
            800,
            &cockpit_with_subagents(),
        );
        let button = gauge(&window);
        button.popup();
        assert!(settle_until(|| button.popover().unwrap().is_visible()));
        let rows = gauge_rows(&button);
        assert_eq!(row(&rows, "Claude"), "3");
        assert_eq!(row(&rows, "pi"), "2");
        assert_eq!(row(&rows, "Codex"), "1");
        assert_eq!(row(&rows, "gemini"), "1");
        assert_eq!(row(&rows, "Live now"), "4");
        assert_eq!(row(&rows, "atlas-api-auth-refactor"), "3");
        assert_eq!(row(&rows, "quill-flaky-tests"), "1");
        assert!(
            rows.iter().all(|(n, _)| n != "ledger-migration-v4"),
            "a session with no live sub-session is not listed"
        );
        assert_eq!(row(&rows, "Spawned in total"), "7");
        let at = |n: &str| rows.iter().position(|(r, _)| r == n).unwrap();
        assert!(at("atlas-api-auth-refactor") < at("quill-flaky-tests"));
        button.popdown();
        window.window.close();
        settle();
    });
}

#[test]
fn unreported_sub_sessions_show_a_dash_never_zero() {
    gtk::test_synced(|| {
        let cockpit: Snapshot =
            serde_json::from_str(include_str!("../tests/fixtures/snapshots/cockpit.json")).unwrap();
        let window = open("io.agentcenter.GaugeDashQA", 1280, 800, &cockpit);
        let button = gauge(&window);
        let rows = gauge_rows(&button);
        assert_eq!(row(&rows, "Live now"), "–");
        assert_eq!(row(&rows, "Spawned in total"), "–");
        assert_eq!(row(&rows, "Claude"), "3");
        assert_eq!(
            button.tooltip_text().as_deref(),
            Some("7 open sessions, live sub-sessions unknown; at least 7 of 40")
        );
        window.window.close();
        settle();
    });
}

#[test]
fn the_gauge_colour_follows_the_ceiling() {
    gtk::test_synced(|| {
        // 38 agent sessions of 40: past 90 %.
        let window = open("io.agentcenter.GaugeLevelQA", 1280, 800, &tipico());
        let button = gauge(&window);
        assert_eq!(gauge_number(&button), "38");
        assert!(button.has_css_class("level-error"));
        window.apply(Ok(Frame {
            snapshot: Arc::new(cockpit_with_subagents()),
            history: None,
        }));
        assert!(settle_until(|| button.has_css_class("level-normal")));
        assert!(!button.has_css_class("level-error"));
        window.window.close();
        settle();
    });
}

#[test]
fn the_popover_opens_from_the_keyboard() {
    gtk::test_synced(|| {
        let window = open(
            "io.agentcenter.GaugeKeyQA",
            1280,
            800,
            &cockpit_with_subagents(),
        );
        let button = gauge(&window);
        // A MenuButton hands focus to its inner toggle: that one is on Tab.
        let toggle = descendants(button.upcast_ref())
            .into_iter()
            .find_map(|w| w.downcast::<gtk::ToggleButton>().ok())
            .expect("the inner toggle");
        assert!(toggle.is_focusable(), "reachable by Tab");
        assert!(button.grab_focus());
        assert!(button.activate(), "Enter or Space");
        assert!(settle_until(|| button.popover().unwrap().is_visible()));
        window.window.close();
        settle();
    });
}

#[test]
fn an_unchanged_snapshot_keeps_the_popover_rows_and_a_change_rebuilds_them() {
    gtk::test_synced(|| {
        let snap = cockpit_with_subagents();
        let window = open("io.agentcenter.GaugeQuietQA", 1280, 800, &snap);
        let button = gauge(&window);
        let first = |b: &gtk::MenuButton| {
            descendants(b.popover().unwrap().upcast_ref())
                .into_iter()
                .find(|w| w.has_css_class("gauge-row"))
                .unwrap()
        };
        let before = first(&button);
        let mut later = snap.clone();
        later.taken_at_ms += 1_000;
        window.apply(Ok(Frame {
            snapshot: Arc::new(later.clone()),
            history: None,
        }));
        assert_eq!(first(&button), before, "same counts: same widgets");
        later.sessions[1].overlay.as_mut().unwrap().subagents_live = Some(2);
        window.apply(Ok(Frame {
            snapshot: Arc::new(later),
            history: None,
        }));
        assert_ne!(first(&button), before, "new counts: rows rebuilt");
        window.window.close();
        settle();
    });
}

/// The RAM bar gauge sits in the machine strip's RAM cell, drawn and inside
/// that cell, with absolute and percent values for tipico-40; the dashed
/// RAM-available column is gone from the memory map.
fn ram_gauge_in_strip_at(id: &str, width: i32, height: i32) {
    let window = open(id, width, height, &tipico());
    let bar = window.strip.ram_gauge_widget().clone();
    assert!(
        settle_until(|| bar.is_mapped() && bar.width() > 20),
        "the gauge is drawn"
    );
    let root = &window.window;
    assert!(
        bar.is_ancestor(&window.strip.widget),
        "in the machine strip"
    );
    assert_eq!(
        bar.tooltip_text().as_deref(),
        Some(
            "RAM 29.3 / 38.0 GiB used (77 %) \u{b7} 8.7 GiB available (23 %) \u{b7} memory stall over limit"
        )
    );
    // The numbers are on screen, not only in the tooltip, in every layout.
    let line = window.strip.ram_gauge_text().clone();
    assert!(
        settle_until(|| line.is_mapped() && line.width() > 20),
        "the numbers line is drawn"
    );
    assert_eq!(
        line.label(),
        "29.3 / 38.0 GiB \u{b7} 77 % used \u{b7} 8.7 GiB (23 %) free"
    );
    assert!(!line.layout().is_ellipsized(), "never cut");
    let l = bounds_in(&line, root);
    let strip_b = bounds_in(&window.strip.widget, root);
    assert!(
        l.x() >= strip_b.x() && l.x() + l.width() <= strip_b.x() + strip_b.width() + 0.5,
        "the line stays inside the strip: {l:?}"
    );
    assert!(
        (bar.value() - 0.771).abs() < 0.001,
        "fill is in use / total"
    );
    let (b, strip) = (bounds_in(&bar, root), bounds_in(&window.strip.widget, root));
    assert!(
        b.x() >= strip.x() && b.x() + b.width() <= strip.x() + strip.width() + 0.5,
        "inside the strip at {width} px: {b:?} in {strip:?}"
    );
    window.window.close();
    settle();
}

#[test]
fn the_ram_gauge_is_in_the_machine_strip_wide() {
    gtk::test_synced(|| ram_gauge_in_strip_at("io.agentcenter.RamGaugeWideQA", 1280, 800));
}

#[test]
fn the_ram_gauge_is_in_the_machine_strip_2x2_when_narrow() {
    gtk::test_synced(|| {
        ram_gauge_in_strip_at("io.agentcenter.RamGaugeNarrowQA", 360, 640);
    });
}

/// Card 93dc0213: the context gauge's cell shows the fullest session's % and
/// name on screen (tipico-40: sable-export-csv at 96.4 %), drawn inside the
/// strip, and states the level in words.
fn context_gauge_in_strip_at(id: &str, width: i32, height: i32) {
    let window = open(id, width, height, &tipico());
    let gauge = window.strip.context();
    let (value, name, button) = (
        gauge.value_label().clone(),
        gauge.name_label().clone(),
        gauge.button.clone(),
    );
    assert!(
        settle_until(|| value.is_mapped()
            && value.width() > 10
            && name.is_mapped()
            && name.width() > 10),
        "the readout and the name are drawn at {width} px"
    );
    assert_eq!(value.label(), "96%");
    assert_eq!(name.label(), "sable-export-csv");
    assert!(!name.layout().is_ellipsized(), "the name is never cut");
    assert!(!value.layout().is_ellipsized());
    assert!(button.is_mapped(), "the ring is drawn");
    assert_eq!(
        button.tooltip_text().as_deref(),
        Some(
            "Context: highest 96% in sable-export-csv, critical (90% or more); \
             10 sessions at or above 75%, 3 at or above 90%"
        )
    );
    let root = &window.window;
    let strip = bounds_in(&window.strip.widget, root);
    for (what, w) in [
        ("value", value.upcast_ref::<gtk::Widget>()),
        ("name", name.upcast_ref()),
        ("ring", button.upcast_ref()),
    ] {
        let b = bounds_in(w, root);
        assert!(
            b.x() >= strip.x()
                && b.x() + b.width() <= strip.x() + strip.width() + 0.5
                && b.y() >= strip.y()
                && b.y() + b.height() <= strip.y() + strip.height() + 0.5,
            "{what} inside the strip at {width} px: {b:?} in {strip:?}"
        );
    }
    window.window.close();
    settle();
}

#[test]
fn the_context_gauge_shows_percent_and_session_name_wide() {
    gtk::test_synced(|| context_gauge_in_strip_at("io.agentcenter.CtxGaugeWideQA", 1280, 800));
}

#[test]
fn the_context_gauge_shows_percent_and_session_name_2x2_when_narrow() {
    gtk::test_synced(|| context_gauge_in_strip_at("io.agentcenter.CtxGaugeNarrowQA", 360, 640));
}

/// Como o `open`, mas com AGENTCENTER_DETAIL=busiest ja armado antes do
/// primeiro quadro: o caminho da captura `capture-gui.sh --detail`.
fn open_detail_at_start(id: &str, width: i32) -> Window {
    adw::init().expect("isolated Broadway display required");
    let app = adw::Application::builder()
        .application_id(id)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    let window = Window::new(&app, false, &Prefs::from_source(None, None), |_| {});
    window.open_at_start.set(Some("busiest".into()));
    window.window.set_default_size(width, 800);
    window.apply(Ok(Frame {
        snapshot: Arc::new(tipico()),
        history: None,
    }));
    window.window.present();
    settle();
    window
}

/// Bug da captura: o detalhe aberto na partida empurrava a pagina vazia do
/// split (recolhido sempre) mesmo com a janela larga, onde o detalhe mora no
/// lugar dos mapas; a janela saia so com a faixa da maquina.
#[test]
fn wide_detail_opened_at_start_is_drawn_in_place_of_the_maps() {
    gtk::test_synced(|| {
        let window = open_detail_at_start("io.agentcenter.DetailStartWideQA", 1280);
        assert!(!window.narrow.get());
        assert!(
            !window.split.shows_content(),
            "wide, no empty page is pushed over the list"
        );
        assert_eq!(
            content_stack(&window).visible_child_name().as_deref(),
            Some("detail")
        );
        let tb = &window.detail.toolbar;
        assert!(
            settle_until(|| tb.is_mapped() && tb.height() > 100),
            "the detail is drawn"
        );
        window.window.close();
        settle();
    });
}

#[test]
fn narrow_detail_opened_at_start_is_the_pushed_page() {
    gtk::test_synced(|| {
        let window = open_detail_at_start("io.agentcenter.DetailStartNarrowQA", 360);
        assert!(window.narrow.get());
        assert!(window.split.shows_content());
        let tb = &window.detail.toolbar;
        assert!(
            settle_until(|| tb.is_mapped() && tb.height() > 100),
            "the detail is drawn"
        );
        window.window.close();
        settle();
    });
}

/// O detalhe aberto na partida larga nao deixa pendencia: depois de
/// estreitar e voltar, selecionar outra sessao nao empurra o detalhe.
#[test]
fn wide_start_detail_leaves_nothing_that_pushes_a_later_selection() {
    gtk::test_synced(|| {
        let window = open_detail_at_start("io.agentcenter.DetailStartLeakQA", 1280);
        let snap = tipico();
        window.window.set_default_size(360, 800);
        assert!(settle_until(|| window.narrow.get()));
        window.split.set_show_content(false);
        settle();
        let other = snap
            .sessions
            .iter()
            .map(|s| s.pid)
            .find(|&p| p != busiest_pid(&snap))
            .unwrap();
        assert!(window.list.select_pid(other));
        settle();
        assert!(
            !window.split.shows_content(),
            "a selection alone does not push the detail"
        );
        window.window.close();
        settle();
    });
}

// ---- 360 px overflow (bug 8b4af569) ------------------------------------------

/// `inner` lies inside `outer` (px, half a px of rounding allowed).
fn inside(inner: &gtk::graphene::Rect, outer: &gtk::graphene::Rect) -> bool {
    inner.x() >= outer.x() - 0.5
        && inner.y() >= outer.y() - 0.5
        && inner.x() + inner.width() <= outer.x() + outer.width() + 0.5
        && inner.y() + inner.height() <= outer.y() + outer.height() + 0.5
}

fn mapped_with_class(window: &Window, class: &str) -> Vec<gtk::Widget> {
    descendants(window.window.upcast_ref())
        .into_iter()
        .filter(|w| w.is_mapped() && w.has_css_class(class))
        .collect()
}

/// What the desktop captures run with, for the length of a test: the app's
/// stylesheet (the widths and paddings are CSS) and an 11 pt DejaVu Sans,
/// named outright and installed by CI (`fonts-dejavu-core`) so the pixel
/// bounds below measure the same glyphs on every machine; `set` checks that
/// the family resolves. The Broadway
/// defaults (no stylesheet, Sans 10) hide the overflow. Tests run one at a
/// time on the main thread; the drop puts both back.
const DESKTOP_FONT: &str = "DejaVu Sans 11";

struct DesktopLook {
    provider: gtk::CssProvider,
    font: Option<gtk::glib::GString>,
}

impl DesktopLook {
    fn set() -> Self {
        adw::init().expect("isolated Broadway display required");
        let provider = gtk::CssProvider::new();
        provider.load_from_string(crate::app::STYLESHEET);
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let settings = gtk::Settings::default().expect("default settings");
        let font = settings.gtk_font_name();
        settings.set_gtk_font_name(Some(DESKTOP_FONT));
        let desc = gtk::pango::FontDescription::from_string(DESKTOP_FONT);
        let family = gtk::Label::new(None)
            .pango_context()
            .load_font(&desc)
            .and_then(|f| f.describe().family())
            .map(|f| f.to_string());
        assert_eq!(
            family.as_deref(),
            Some("DejaVu Sans"),
            "{DESKTOP_FONT} does not resolve here: install fonts-dejavu-core"
        );
        Self { provider, font }
    }
}

impl Drop for DesktopLook {
    fn drop(&mut self) {
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_remove_provider_for_display(&display, &self.provider);
        }
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_font_name(self.font.as_deref());
        }
    }
}

/// Waits for the paned to lay its top out at the position it was given. A
/// breakpoint that flips the maps to strips lands inside a layout pass, so the
/// top and the labels take their final size one frame later, which Broadway
/// (no browser acknowledging frames) delays by about a second.
fn settle_paned(window: &Window) {
    let paned = descendants(window.window.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Paned>().ok())
        .expect("the window has a paned");
    settle_until(|| {
        paned
            .start_child()
            .is_some_and(|c| c.height() == paned.position())
    });
    settle();
}

/// The content's least width at 360 px: a minimum above the window's makes
/// GTK allocate past the edge, which clips the header chip and the dropdowns.
#[test]
fn at_360_the_window_content_does_not_overflow_sideways() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let window = open("io.agentcenter.Narrow360WidthQA", 360, 640, &tipico());
        settle_paned(&window);
        let content = window.window.content().expect("window content");
        let (least, _, _, _) = content.measure(gtk::Orientation::Horizontal, -1);
        assert!(
            least <= 360,
            "content needs {least} px at the least, 360 given"
        );
        assert!(
            content.width() <= 360,
            "content allocated {} px in a 360 px window",
            content.width()
        );
        let problems = crate::measure::overflow(window.window.upcast_ref(), Some(360));
        assert!(problems.is_empty(), "overflow: {problems:?}");
        window.window.close();
        settle();
    });
}

#[test]
fn at_360_the_waiting_chip_and_both_dropdowns_lie_inside_the_window() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let window = open("io.agentcenter.Narrow360ChipsQA", 360, 640, &tipico());
        settle_paned(&window);
        let root = window.window.clone();
        let whole = gtk::graphene::Rect::new(0.0, 0.0, 360.0, 640.0);
        let stack = content_stack(&window);
        let region = bounds_in(&stack, &root);
        let chips = mapped_with_class(&window, "chip");
        assert!(!chips.is_empty(), "the waiting chips are shown");
        let mut clipped = Vec::new();
        for chip in &chips {
            let b = bounds_in(chip, &root);
            if !inside(&b, &whole) || !inside(&b, &region) {
                clipped.push(format!(
                    "chip {b:?} outside the window {whole:?} or the maps region {region:?}"
                ));
            }
        }
        let drops: Vec<gtk::Widget> = descendants(root.upcast_ref())
            .into_iter()
            .filter(|w| w.is_mapped() && w.is::<gtk::DropDown>())
            .collect();
        assert_eq!(drops.len(), 2, "sort and agent dropdowns are shown");
        for d in &drops {
            let b = bounds_in(d, &root);
            if !inside(&b, &whole) {
                clipped.push(format!("dropdown {b:?} outside the window {whole:?}"));
            }
            let bar = bounds_in(&window.switcher_bar, &root);
            if window.switcher_bar.reveals() && b.y() + b.height() > bar.y() {
                clipped.push(format!("dropdown {b:?} under the switcher bar {bar:?}"));
            }
        }
        assert!(clipped.is_empty(), "{}", clipped.join("; "));
        window.window.close();
        settle();
    });
}

/// The detail's header bar (Back, the empty title, Handoff and Compact copies)
/// belongs to the pushed page: the wide pane keeps it hidden, so the empty
/// title never shows beside the list.
#[test]
fn the_wide_detail_pane_hides_the_page_header() {
    gtk::test_synced(|| {
        let window = open_detail_at_start("io.agentcenter.WideDetailHeaderQA", 1280);
        let tb = &window.detail.toolbar;
        assert!(
            settle_until(|| tb.is_mapped() && tb.height() > 100),
            "the detail is drawn"
        );
        assert!(
            !tb.reveals_top_bars(),
            "the wide detail pane shows the pushed page's header bar"
        );
        window.window.close();
        settle();
    });
}

/// The pushed detail page at 360 px: its least width is what the window asks
/// for, so a minimum above 360 pushes the whole window past its edge.
#[test]
fn at_360_the_detail_page_does_not_overflow_sideways() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let window = open_detail_at_start("io.agentcenter.Narrow360DetailQA", 360);
        let tb = &window.detail.toolbar;
        assert!(
            settle_until(|| tb.is_mapped() && tb.height() > 100),
            "the detail is drawn"
        );
        // The push slides the page in; checked mid-slide it lies off the edge.
        let root = window.window.clone();
        assert!(
            settle_until(|| tb.compute_bounds(&root).is_some_and(|b| b.x().abs() < 0.5)),
            "the detail page came to rest on the window"
        );
        // The bin lays the page out a frame after it comes to rest; until then
        // everything in it is 0 px wide and the walk below sees nothing.
        let page = descendants(tb.upcast_ref())
            .into_iter()
            .find(|w| w.is::<adw::BreakpointBin>())
            .expect("the detail's breakpoint bin");
        assert!(
            settle_until(|| page.width() > 0),
            "the detail page is laid out"
        );
        let (least, _, _, _) = tb.measure(gtk::Orientation::Horizontal, -1);
        assert!(
            least <= 360,
            "detail page needs {least} px at the least, 360 given"
        );
        let content = window.window.content().expect("window content");
        let (least, _, _, _) = content.measure(gtk::Orientation::Horizontal, -1);
        assert!(
            least <= 360,
            "content needs {least} px at the least with the detail shown, 360 given"
        );
        let problems = crate::measure::overflow(window.window.upcast_ref(), Some(360));
        assert!(problems.is_empty(), "overflow: {problems:?}");
        window.window.close();
        settle();
    });
}

#[test]
fn at_360_the_memory_strip_is_visible() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let window = open("io.agentcenter.Narrow360MemoryQA", 360, 640, &tipico());
        settle_paned(&window);
        let root = window.window.clone();
        let region = bounds_in(&content_stack(&window), &root);
        let bars = mapped_with_class(&window, "memory-bar");
        assert_eq!(bars.len(), 1, "the memory strip is mapped");
        let b = bounds_in(&bars[0], &root);
        assert!(b.height() > 0.0, "the memory strip has a height");
        assert!(
            inside(&b, &region),
            "memory strip {b:?} clipped by the maps region {region:?}"
        );
        window.window.close();
        settle();
    });
}

/// The strips' height follows the chips: a snapshot that brings three chips
/// and an alert line after the window settled on a lighter one must still
/// leave the memory strip whole (the capture's tipico-40 lost it).
#[test]
fn at_360_the_memory_strip_stays_visible_when_a_busier_snapshot_arrives() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let calm: Snapshot =
            serde_json::from_str(include_str!("../tests/fixtures/snapshots/vazio.json")).unwrap();
        let window = open("io.agentcenter.Narrow360BusyQA", 360, 640, &calm);
        settle_paned(&window);
        window.apply(Ok(Frame {
            snapshot: Arc::new(tipico()),
            history: None,
        }));
        settle_paned(&window);
        let root = window.window.clone();
        let region = bounds_in(&content_stack(&window), &root);
        let bars = mapped_with_class(&window, "memory-bar");
        assert_eq!(bars.len(), 1, "the memory strip is mapped");
        let b = bounds_in(&bars[0], &root);
        assert!(b.height() > 0.0, "the memory strip has a height");
        assert!(
            inside(&b, &region),
            "memory strip {b:?} clipped by the maps region {region:?}"
        );
        window.window.close();
        settle();
    });
}

/// With no token spend the Tokens strip says "no sessions" and nothing else,
/// and at 360 x 640 its row is the height the memory strip loses in the
/// capture's real fonts: the strip is left out until there is spend to show.
#[test]
fn at_360_the_tokens_strip_is_left_out_while_it_has_no_sessions() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let window = open("io.agentcenter.Narrow360NoTokensQA", 360, 640, &tipico());
        settle_paned(&window);
        let titled = |text: &str| {
            descendants(window.window.upcast_ref())
                .into_iter()
                .filter(|w| w.is_mapped())
                .filter_map(|w| w.downcast::<gtk::Label>().ok())
                .any(|l| l.label() == text)
        };
        assert!(!titled("Tokens"), "the empty Tokens strip takes a row");
        assert!(titled("Memory"), "the Memory strip stays");
        window.window.close();
        settle();
    });
}

/// The breakpoint bin hands its child the height of its reference (wide)
/// layout, which the stacked strips outgrow: Adwaita warns "requested 177 px,
/// 108 px available" and the bin clips the Memory heading and bar while the
/// paned leaves blank room below. The child gets what it asks for.
#[test]
fn at_360_the_maps_content_gets_the_height_it_asks_for() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let window = open("io.agentcenter.Narrow360AskedQA", 360, 640, &tipico());
        settle_paned(&window);
        let bin = window
            .map
            .widget()
            .first_child()
            .expect("the breakpoint bin");
        let inner = bin
            .downcast::<adw::BreakpointBin>()
            .unwrap()
            .child()
            .unwrap();
        let asked = inner.measure(gtk::Orientation::Vertical, inner.width()).1;
        assert!(
            inner.height() >= asked,
            "the maps content is {} px tall and asks for {asked}",
            inner.height()
        );
        window.window.close();
        settle();
    });
}

fn paned_of(window: &Window) -> gtk::Paned {
    descendants(window.window.upcast_ref())
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Paned>().ok())
        .expect("the window has a paned")
}

/// The top of the paned is what the strips ask for (never the 64 px default
/// the narrow mode starts from), and the list keeps its chrome. Checked after
/// a busier snapshot, the case the re-clamp on every map render is for.
#[test]
fn at_360_the_paned_top_is_at_least_what_the_maps_need() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let calm: Snapshot =
            serde_json::from_str(include_str!("../tests/fixtures/snapshots/vazio.json")).unwrap();
        let window = open("io.agentcenter.Narrow360PanedQA", 360, 640, &calm);
        settle_paned(&window);
        window.apply(Ok(Frame {
            snapshot: Arc::new(tipico()),
            history: None,
        }));
        settle_paned(&window);
        let paned = paned_of(&window);
        let need = window.map.needed_height(content_stack(&window).width());
        assert!(
            need > STRIPS_TOP_PX,
            "the strips need {need} px, no more than the {STRIPS_TOP_PX} px default"
        );
        assert!(
            paned.position() >= need,
            "the paned top is {} px, the maps need {need}",
            paned.position()
        );
        assert!(
            paned.position() <= paned.height() - LIST_CHROME_PX,
            "the paned top {} px leaves the list less than its {LIST_CHROME_PX} px chrome of {}",
            paned.position(),
            paned.height()
        );
        window.window.close();
        settle();
    });
}

/// tipico-40 with names and an agent type no 360 px window has room for:
/// long names unbreakable at any point, so only ellipsizing keeps them in.
fn long_names() -> Snapshot {
    let mut snap = tipico();
    for (i, s) in snap.sessions.iter_mut().enumerate() {
        if let Some(o) = s.overlay.as_mut() {
            o.name = Some(format!("{}{i}", "n".repeat(90)));
        }
        if i == 0 {
            s.agent = "a".repeat(70);
        }
    }
    snap
}

/// Names of any length: the chip's name and the dropdown's items give way,
/// since a minimum above the window's makes GTK allocate past the edge.
#[test]
fn at_360_long_names_do_not_widen_the_window() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let window = open("io.agentcenter.Narrow360LongQA", 360, 640, &long_names());
        settle_paned(&window);
        let root = window.window.clone();
        let whole = gtk::graphene::Rect::new(0.0, 0.0, 360.0, 640.0);
        let content = window.window.content().expect("window content");
        let (least, _, _, _) = content.measure(gtk::Orientation::Horizontal, -1);
        assert!(
            least <= 360,
            "content needs {least} px at the least, 360 given"
        );
        // The longest list heading, beside the order and the subheading.
        window.list.set_sort_key(SortKey::SinceStart);
        settle_paned(&window);
        let (least, _, _, _) = content.measure(gtk::Orientation::Horizontal, -1);
        assert!(
            least <= 360,
            "content needs {least} px at the least with the since-start heading, 360 given"
        );
        assert!(
            !mapped_with_class(&window, "chip").is_empty(),
            "the waiting chips are shown"
        );
        // The stock item factory sizes the button to its longest item (155 and
        // 122 px here, 57 and 53 with the ellipsizing label); the two together
        // keep to half the window, or the order bar has no room for its label.
        let least_widths: i32 = descendants(root.upcast_ref())
            .into_iter()
            .filter(|w| w.is_mapped() && w.is::<gtk::DropDown>())
            .map(|d| d.measure(gtk::Orientation::Horizontal, -1).0)
            .sum();
        assert!(
            least_widths <= 180,
            "the dropdowns need {least_widths} px at the least, half the window is 180"
        );
        let mut clipped = Vec::new();
        for w in descendants(root.upcast_ref())
            .into_iter()
            .filter(|w| w.is_mapped() && (w.has_css_class("chip") || w.is::<gtk::DropDown>()))
        {
            let b = bounds_in(&w, &root);
            if !inside(&b, &whole) {
                clipped.push(format!("{} {b:?} outside {whole:?}", w.type_().name()));
            }
        }
        assert!(clipped.is_empty(), "{}", clipped.join("; "));
        window.window.close();
        settle();
    });
}

/// The compact cells take 6 px all round: the 2 x 2 grid has no room for the
/// 12 px sides of the wide row.
#[test]
fn at_360_the_machine_strip_cells_take_the_compact_padding() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let window = open("io.agentcenter.Narrow360PaddingQA", 360, 640, &tipico());
        settle_paned(&window);
        let strip = descendants(window.window.upcast_ref())
            .into_iter()
            .find(|w| w.is_mapped() && w.has_css_class("machine-strip"))
            .expect("the machine strip");
        assert!(strip.has_css_class("compact"), "the strip is compact");
        // A widget's width() is its content box; the allocation adds the padding.
        let cell = strip.first_child().expect("a strip cell");
        let outer = cell.compute_bounds(&strip).expect("laid out");
        let (across, down) = (
            outer.width() - cell.width() as f32,
            outer.height() - cell.height() as f32,
        );
        assert!(
            (across - 12.0).abs() < 0.5 && (down - 12.0).abs() < 0.5,
            "the cell's padding takes {across} x {down} px, the compact padding is 6 + 6 each way"
        );
        window.window.close();
        settle();
    });
}

/// Session rows that lie whole inside the list's scrolled viewport.
fn whole_list_rows(window: &Window) -> usize {
    let rows: Vec<gtk::Widget> = mapped_with_class(window, "session-row");
    let Some(scrolled) = rows
        .first()
        .and_then(|r| r.ancestor(gtk::ScrolledWindow::static_type()))
    else {
        return 0;
    };
    let viewport = bounds_in(&scrolled, &window.window);
    rows.iter()
        .filter(|r| inside(&bounds_in(*r, &window.window), &viewport))
        .count()
}

/// The strips' 4 px padding is the row: with the 16 px of the wide page the
/// list under the strips shows one whole session instead of two.
#[test]
fn at_360x640_the_list_keeps_2_whole_rows() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let window = open("io.agentcenter.Narrow360RowsQA", 360, 640, &tipico());
        settle_paned(&window);
        let whole = whole_list_rows(&window);
        assert!(
            whole >= 2,
            "the list shows {whole} whole session rows at 360 x 640, 2 fit"
        );
        window.window.close();
        settle();
    });
}

/// tipico-40 with every session that reports a state waiting for its user:
/// a chip each, more than a 360 x 640 top has room for.
fn all_waiting() -> Snapshot {
    let mut snap = tipico();
    for s in &mut snap.sessions {
        if let Some(o) = s.overlay.as_mut() {
            o.status = Some(agentcenter_core::overlay::OverlayStatus::Blocked);
        }
    }
    snap
}

/// A chip per waiting session asks the strips for more height than the window has:
/// the top stops where the list's chrome begins, and the list scrolls its rows.
#[test]
fn at_360_many_waiting_chips_leave_the_list_its_chrome() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let window = open(
            "io.agentcenter.Narrow360WaitingQA",
            360,
            640,
            &all_waiting(),
        );
        settle_paned(&window);
        let paned = paned_of(&window);
        let cap = paned.height() - LIST_CHROME_PX;
        let need = window.map.needed_height(content_stack(&window).width());
        assert!(
            need > cap,
            "the chips ask for {need} px, no more than the {cap} px the top may take"
        );
        assert!(
            paned.position() <= cap,
            "the paned top is {} px, past the {cap} px that leave the list its {LIST_CHROME_PX} px chrome",
            paned.position()
        );
        window.window.close();
        settle();
    });
}

/// Back to a wide window the strips let go: no strips class, and the bin
/// drops the height the strips had it ask for, or the maps keep a floor
/// as tall as the stacked strips.
#[test]
fn from_360_to_1280x800_the_strips_let_go_of_the_bin() {
    gtk::test_synced(|| {
        let _font = DesktopLook::set();
        let window = open("io.agentcenter.Narrow360GrowQA", 360, 640, &tipico());
        settle_paned(&window);
        let page = window.map.widget().clone();
        let bin = page.first_child().expect("the breakpoint bin");
        assert!(page.has_css_class("strips"), "360 x 640 has the strips");
        assert!(bin.size_request().1 > 1, "the strips raised the bin");
        window.window.set_default_size(1280, 800);
        assert!(
            settle_until(|| !page.has_css_class("strips")),
            "1280 x 800 keeps the strips class"
        );
        settle_paned(&window);
        assert_eq!(bin.size_request(), (280, 1), "the bin's size request");
        window.window.close();
        settle();
    });
}

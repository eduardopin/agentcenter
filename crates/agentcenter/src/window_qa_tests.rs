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
            .find(|b| b.label().as_deref() == Some("View session"))
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
            window.header.widget.is_visible(),
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
fn cc2_native_counter_zero_clear_detail_disabled_back() {
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
        let widgets = descendants(window.window.upcast_ref());
        let counter = widgets
            .iter()
            .filter_map(|w| w.clone().downcast::<gtk::Button>().ok())
            .find(|b| {
                b.has_css_class("counter-filter")
                    && descendants(b.upcast_ref()).iter().any(|w| {
                        w.downcast_ref::<gtk::Label>()
                            .is_some_and(|l| l.label().contains("blocked"))
                    })
            })
            .expect("blocked counter exists");
        assert!(counter.grab_focus());
        assert!(counter.activate());
        settle();
        assert!(window.list.has_filter());
        let clear = descendants(window.window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Clear"))
            .unwrap();
        assert_eq!(
            gtk::prelude::GtkWindowExt::focus(&window.window),
            Some(clear.clone().upcast())
        );
        assert!(!window.split.shows_content());
        assert!(clear.activate());
        settle();
        assert!(!window.list.has_filter());
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
            .find(|b| b.label().as_deref() == Some("Focus terminal"))
            .unwrap();
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

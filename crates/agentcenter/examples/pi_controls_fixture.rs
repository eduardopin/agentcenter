//! Regression: real widgets must reach native persistence, not accepted-only success.
#![allow(dead_code)]
use adw::prelude::*;
use gtk::glib;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};
#[path = "../src/actions_ui/session_control.rs"]
mod session_control;
fn button(root: &gtk::Box, name: &str) -> gtk::Button {
    let mut child = root.first_child();
    while let Some(w) = child {
        if let Ok(b) = w.clone().downcast::<gtk::Button>()
            && b.label().as_deref() == Some(name)
        {
            return b;
        }
        child = w.next_sibling();
    }
    panic!("missing button {name}");
}
fn dialog_button(root: &gtk::Widget, label: &str) -> Option<gtk::Button> {
    if let Some(button) = root.downcast_ref::<gtk::Button>()
        && button.label().as_deref() == Some(label)
    {
        return Some(button.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(button) = dialog_button(&widget, label) {
            return Some(button);
        }
        child = widget.next_sibling();
    }
    None
}
fn labels(root: &gtk::Box) -> String {
    let mut result = String::new();
    let mut child = root.first_child();
    while let Some(w) = child {
        if let Ok(l) = w.clone().downcast::<gtk::Label>() {
            result.push_str(&l.text());
        }
        child = w.next_sibling();
    }
    result
}
#[derive(Default)]
struct CaptureState {
    requested: RefCell<Option<&'static str>>,
    completed: Cell<Option<&'static str>>,
}

fn capture_ready(
    window: &adw::ApplicationWindow,
    state: &CaptureState,
    name: &'static str,
) -> bool {
    if state.completed.get() == Some(name) {
        state.completed.set(None);
        return true;
    }
    if state.requested.borrow().is_none() {
        state.requested.replace(Some(name));
        window.queue_draw();
    }
    false
}
fn capture(window: &adw::ApplicationWindow, name: &str) -> bool {
    let Ok(dir) = std::env::var("CC3_CAPTURE_DIR") else {
        return true;
    };
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(
        &snapshot,
        f64::from(window.width()),
        f64::from(window.height()),
    );
    let Some(node) = snapshot.to_node() else {
        return false;
    };
    let texture = window.renderer().unwrap().render_texture(&node, None);
    std::fs::create_dir_all(&dir).unwrap();
    let theme = std::env::var("CC3_THEME").unwrap_or_else(|_| "light".into());
    let path = format!(
        "{dir}/{}x{}-{theme}-{name}.png",
        window.width(),
        window.height()
    );
    texture.save_to_png(&path).unwrap();
    println!("CAPTURE {path}");
    true
}
/// Captures a frame right after it is painted, when requested by name.
fn install_capture(window: &adw::ApplicationWindow) -> Rc<CaptureState> {
    let captures = Rc::new(CaptureState::default());
    let capture_state = captures.clone();
    window.connect_realize(move |window| {
        let state = capture_state.clone();
        let window = window.clone();
        window
            .clone()
            .frame_clock()
            .unwrap()
            .connect_after_paint(move |_| {
                let Some(name) = state.requested.borrow_mut().take() else {
                    return;
                };
                let (state, window) = (state.clone(), window.clone());
                // Match the application's screenshot probe: outside painting, before
                // another timeout/feed can invalidate the freshly allocated render node.
                glib::idle_add_local_full(glib::Priority::HIGH, move || {
                    if capture(&window, name) {
                        state.completed.set(Some(name));
                    } else {
                        state.requested.replace(Some(name));
                        window.queue_draw();
                    }
                    glib::ControlFlow::Break
                });
            });
    });
    captures
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--scenario") {
        scenario::run();
        return;
    }
    let metadata: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
    let mut native = agentcenter_core::Session::aggregate(
        "pi".into(),
        agentcenter_core::adapters::AgentRole::Agent,
        metadata["pid"].as_u64().unwrap() as u32,
        1,
        vec![],
    );
    native.starttime = Some(
        metadata["process_starttime"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
    );
    native.overlay = Some(agentcenter_core::overlay::SessionOverlay {
        name: Some("Synthetic test session".into()),
        agent_session_id: Some(metadata["session_id"].as_str().unwrap().into()),
        ..Default::default()
    });
    let app = adw::Application::builder()
        .application_id("dev.agentcenter.SyntheticControls")
        .build();
    app.connect_activate(move |app| {
        let style = adw::StyleManager::default();
        let dark = std::env::var("CC3_THEME").as_deref() == Ok("dark");
        style.set_color_scheme(if dark {
            adw::ColorScheme::ForceDark
        } else {
            adw::ColorScheme::ForceLight
        });
        assert_eq!(style.is_dark(), dark);
        println!("THEME dark={}", style.is_dark());
        let controls = session_control::SessionControls::new(true);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.append(&gtk::Label::new(Some("Synthetic test session")));
        content.append(&controls.widget);
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Synthetic test session")
            .default_width(std::env::var("CC3_WIDTH").map_or(360, |v| v.parse().unwrap()))
            .default_height(std::env::var("CC3_HEIGHT").map_or(640, |v| v.parse().unwrap()))
            .content(&content)
            .build();
        let captures = install_capture(&window);
        window.present();
        controls.set_session(Some(&native));
        let root = controls.widget.clone();
        let stage = Rc::new(Cell::new(0u8));
        let settled = Cell::new(0u8);
        let start = Instant::now();
        glib::timeout_add_local(Duration::from_millis(100), move || {
            assert!(
                start.elapsed() < Duration::from_secs(25),
                "GUI timed out stage {}: {}",
                stage.get(),
                labels(&root)
            );
            match stage.get() {
                0 if button(&root, "Handoff…").is_visible() => {
                    button(&root, "Handoff…").emit_clicked();
                    stage.set(1);
                }
                1 | 5 => {
                    if let Some(d) = window.visible_dialog() {
                        let d = d.downcast::<adw::AlertDialog>().unwrap();
                        // Visible/mapped may precede allocation, especially on headless Broadway.
                        // Never label a background-only frame as a confirmation capture.
                        if !d.is_mapped() || d.width() <= 0 || d.height() <= 0 {
                            window.queue_draw();
                            settled.set(0);
                            return glib::ControlFlow::Continue;
                        }
                        settled.set(settled.get() + 1);
                        if settled.get() < 4
                            || !capture_ready(
                                &window,
                                &captures,
                                if stage.get() == 1 {
                                    "handoff-confirm"
                                } else {
                                    "compact-confirm"
                                },
                            )
                        {
                            return glib::ControlFlow::Continue;
                        }
                        settled.set(0);
                        assert!(d.body().contains("Does not clear"));
                        assert_eq!(d.default_response().as_deref(), Some("cancel"));
                        if stage.get() == 1 {
                            assert!(
                                d.body()
                                    .contains("Sends projected session text to the provider")
                            );
                        }
                        let label = if stage.get() == 1 {
                            "Handoff"
                        } else {
                            "Compact"
                        };
                        let confirm =
                            dialog_button(d.upcast_ref(), label).expect("native confirm button");
                        assert!(confirm.grab_focus());
                        assert_eq!(
                            gtk::prelude::GtkWindowExt::focus(&window),
                            Some(confirm.clone().upcast())
                        );
                        d.connect_response(None, |_, response| {
                            println!("DIALOG_RESPONSE {response}")
                        });
                        confirm.connect_clicked(|_| println!("CONFIRM_CLICKED"));
                        assert!(
                            confirm.activate(),
                            "keyboard action path must activate confirmation"
                        );
                        println!(
                            "CONFIRM {:?} default=cancel focus=confirm activation=GTK",
                            d.heading()
                        );
                        stage.set(stage.get() + 1);
                    }
                }
                2 if labels(&root).starts_with("Saved:") => {
                    assert!(labels(&root).contains("Persisted at:"));
                    settled.set(settled.get() + 1);
                    if settled.get() < 4 || !capture_ready(&window, &captures, "handoff-completed")
                    {
                        return glib::ControlFlow::Continue;
                    }
                    settled.set(0);
                    println!("HANDOFF {}", labels(&root));
                    // No Refresh button (CC7): the panel rediscovers by itself.
                    assert!(!labels(&root).contains("Refresh"));
                    stage.set(3);
                }
                3 if button(&root, "Compact…").is_visible() => {
                    stage.set(4);
                }
                4 => {
                    button(&root, "Compact…").emit_clicked();
                    stage.set(5);
                }
                6 if labels(&root).starts_with("Native compact persisted:") => {
                    assert!(labels(&root).contains("Persisted at:"));
                    settled.set(settled.get() + 1);
                    if settled.get() < 4 || !capture_ready(&window, &captures, "compact-completed")
                    {
                        return glib::ControlFlow::Continue;
                    }
                    println!("COMPACT {}", labels(&root));
                    window.application().unwrap().quit();
                    return glib::ControlFlow::Break;
                }
                _ => {}
            }
            glib::ControlFlow::Continue
        });
        // The panel callbacks retain its state; keep the public handle alive for this fixture.
        std::mem::forget(controls);
    });
    app.run_with_args::<&str>(&[]);
}

/// Scripted scenario for captures and injected failure (CC7). Synthetic only:
/// no Pi, no herdr, no socket; discovery and the pickup adapter are fakes and
/// the stage advances only after the driver captured the previous one.
///
/// `pi_controls_fixture --scenario` walks: section absent with its reason
/// (Codex), section present, the pickup dialog, each of the six stages,
/// a failure injected at stage 3 and an unknown result at stage 5, printing
/// the Tab order where controls are shown. Captures go to `CC3_CAPTURE_DIR`.
mod scenario {
    use super::*;
    use agentcenter_core::control::{Artifact, State, pickup::OldSession};
    use session_control::{
        Action, Agent, Backend, PickupPort, Progress, Request, STAGES, Session, StageMark,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Highest stage index the scripted adapter may report.
    static GATE: AtomicUsize = AtomicUsize::new(0);

    fn discovery(native: &agentcenter_core::Session) -> Result<(Backend, Session), &'static str> {
        let key = native
            .overlay
            .as_ref()
            .and_then(|o| o.agent_session_id.clone())
            .ok_or("identity")?;
        let session = Session {
            agent: Agent::Pi,
            identity: key.clone(),
            revision: "1".into(),
            name: format!("Pi (pid {})", native.pid),
            handoff_destination: "/synthetic/handoffs".into(),
            expected: serde_json::json!({
                "managerId": "synthetic", "revision": "1", "leafId": null,
                "sessionId": format!("{key}-old"),
                "sessionFile": format!("/synthetic/sessions/{key}.jsonl"),
            }),
            handoff_disabled: None,
            compact_disabled: None,
            pickup_disabled: None,
            cancel_supported: false,
        };
        let herdr = agentcenter_core::control::herdr::Endpoint {
            socket: "/synthetic/herdr.sock".into(),
            server_pid: 0,
            server_starttime: 0,
        };
        Ok((Backend::Claude(herdr), session))
    }

    /// `scenario-ok`, `scenario-fail-<n>`, `scenario-unknown-<n>`.
    struct Port {
        request: Request,
        reads: usize,
    }
    impl Port {
        fn at(&self, reads: usize) -> Progress {
            let key = self.request.session.identity.as_str();
            let stop = key.rsplit('-').next().and_then(|n| n.parse::<usize>().ok());
            let index = reads
                .min(GATE.load(Ordering::SeqCst))
                .min(stop.unwrap_or(5));
            let state = match stop {
                Some(n) if index == n && key.contains("-fail-") => State::Failed,
                Some(n) if index == n => State::Unknown,
                _ if index == 5 => State::Completed,
                _ if index == 0 => State::Accepted,
                _ => State::Running,
            };
            let stage = STAGES[index];
            let s = &self.request.session.expected;
            Progress {
                operation_id: self.request.operation_id.clone(),
                state,
                stage,
                code: if state == State::Failed {
                    "clear_failed"
                } else {
                    "synthetic"
                },
                old_session: OldSession {
                    session_id: s["sessionId"].as_str().unwrap().into(),
                    session_file: s["sessionFile"].as_str().unwrap().into(),
                },
                handoff: (index >= 1).then(|| Artifact {
                    path: format!("/synthetic/handoffs/nonce-{}.md", self.request.operation_id)
                        .into(),
                    size: 2048,
                    sha256: "ab".repeat(32),
                    modified_ms: 1_760_000_000_000,
                }),
                new_session_id: (state == State::Completed).then(|| format!("{key}-new")),
            }
        }
    }
    impl PickupPort for Port {
        fn start(&mut self, _: &str) -> Result<Progress, &'static str> {
            Ok(self.at(0))
        }
        fn status(&mut self, _: &str) -> Result<Progress, &'static str> {
            self.reads += 1;
            Ok(self.at(self.reads))
        }
    }
    fn connect(
        _: &Backend,
        _: &agentcenter_core::Session,
        request: &Request,
    ) -> Result<Box<dyn PickupPort>, &'static str> {
        Ok(Box::new(Port {
            request: request.clone(),
            reads: 0,
        }))
    }

    fn native(agent: &str, key: &str) -> agentcenter_core::Session {
        let mut s = agentcenter_core::Session::aggregate(
            agent.into(),
            agentcenter_core::adapters::AgentRole::Agent,
            4242,
            1,
            vec![],
        );
        s.starttime = Some(4242);
        s.overlay = Some(agentcenter_core::overlay::SessionOverlay {
            name: Some("Synthetic test session".into()),
            agent_session_id: Some(key.into()),
            ..Default::default()
        });
        s
    }

    /// Tab order from the window start, as labels, until it wraps.
    fn tab_order(window: &adw::ApplicationWindow) -> Vec<String> {
        gtk::prelude::GtkWindowExt::set_focus(window, None::<&gtk::Widget>);
        let mut out = Vec::new();
        for _ in 0..16 {
            if !window.child_focus(gtk::DirectionType::TabForward) {
                break;
            }
            let Some(w) = gtk::prelude::GtkWindowExt::focus(window) else {
                break;
            };
            let name = w
                .downcast_ref::<gtk::Button>()
                .and_then(|b| b.label())
                .map(String::from)
                .or_else(|| {
                    w.downcast_ref::<gtk::Label>()
                        .map(|l| format!("text:{}", l.text()))
                })
                .unwrap_or_else(|| w.type_().name().to_owned());
            if out.first() == Some(&name) {
                break;
            }
            out.push(name);
        }
        out
    }

    fn css(style: &adw::StyleManager) {
        let provider = gtk::CssProvider::new();
        let follow = |p: &gtk::CssProvider, s: &adw::StyleManager| {
            p.set_prefers_color_scheme(if s.is_dark() {
                gtk::InterfaceColorScheme::Dark
            } else {
                gtk::InterfaceColorScheme::Light
            });
        };
        follow(&provider, style);
        provider.load_from_string(concat!(
            include_str!("../resources/tokens.css"),
            include_str!("../resources/summary.css"),
            include_str!("../resources/sessions.css"),
        ));
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Step {
        Absent,
        Present,
        Dialog(usize),
        Stage(usize),
        Wait(usize),
        Failure,
        Unknown,
    }
    const PICKUP: &str = "Handoff + new session…";

    pub fn run() {
        let app = adw::Application::builder()
            .application_id("dev.agentcenter.SyntheticScenario")
            .build();
        app.connect_activate(|app| {
            let style = adw::StyleManager::default();
            let dark = std::env::var("CC3_THEME").as_deref() == Ok("dark");
            style.set_color_scheme(if dark {
                adw::ColorScheme::ForceDark
            } else {
                adw::ColorScheme::ForceLight
            });
            css(&style);
            println!("THEME dark={}", style.is_dark());
            // Captures show settled states: Broadway without a viewer paints
            // eased transitions (focus ring) mid-way. Runs without captures
            // keep the animated path.
            if std::env::var_os("CC3_CAPTURE_DIR").is_some()
                && let Some(settings) = gtk::Settings::default()
            {
                settings.set_gtk_enable_animations(false);
            }
            let controls = session_control::SessionControls::new(true);
            controls.set_discovery(discovery);
            controls.set_pickup(connect);
            controls.set_poll(Duration::from_millis(50));
            let face = gtk::Box::new(gtk::Orientation::Vertical, 8);
            face.add_css_class("detail-face");
            let name = gtk::Label::new(Some("Synthetic test session"));
            name.add_css_class("detail-name");
            name.set_xalign(0.0);
            face.append(&name);
            face.append(&controls.widget);
            let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
            page.add_css_class("session-detail");
            page.append(&face);
            let scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&page)
                .vexpand(true)
                .build();
            let window = adw::ApplicationWindow::builder()
                .application(app)
                .title("Synthetic test session")
                .default_width(std::env::var("CC3_WIDTH").map_or(360, |v| v.parse().unwrap()))
                .default_height(std::env::var("CC3_HEIGHT").map_or(640, |v| v.parse().unwrap()))
                .content(&scroller)
                .build();
            let captures = install_capture(&window);
            window.present();
            controls.set_session(Some(&native("codex", "scenario-codex")));
            let root = controls.widget.clone();
            let step = Cell::new(Step::Absent);
            let settled = Cell::new(0u8);
            let printed = Cell::new(false);
            let waited = Cell::new(0u8);
            let handled: RefCell<Option<adw::AlertDialog>> = RefCell::new(None);
            let start = Instant::now();
            // Waits a few ticks for layout, then captures `name` once.
            let shot = move |window: &adw::ApplicationWindow, name: &'static str| {
                settled.set(settled.get() + 1);
                if settled.get() < 4 || !capture_ready(window, &captures, name) {
                    return false;
                }
                settled.set(0);
                true
            };
            let open_pickup = |root: &gtk::Box| {
                let b = button(root, PICKUP);
                b.is_visible().then(|| b.emit_clicked()).is_some()
            };
            glib::timeout_add_local(Duration::from_millis(100), move || {
                assert!(
                    start.elapsed() < Duration::from_secs(60),
                    "scenario timed out at {:?}: {} | dialog={:?} buttons={:?}",
                    step.get(),
                    labels(&root),
                    window.visible_dialog().map(|d| d.is_mapped()),
                    session_control_buttons(&root)
                );
                let marks = controls.stage_marks();
                match step.get() {
                    Step::Absent => {
                        assert!(session_control_buttons(&root).is_empty());
                        if labels(&root).contains("no control channel")
                            && shot(&window, "absent-codex")
                        {
                            println!("ABSENT {}", labels(&root));
                            controls.set_session(Some(&native("pi", "scenario-ok")));
                            step.set(Step::Present);
                        }
                    }
                    Step::Present => {
                        if button(&root, PICKUP).is_visible() {
                            if !printed.replace(true) {
                                println!("TABORDER present: {}", tab_order(&window).join(" > "));
                                window.set_focus_visible(true);
                                button(&root, "Handoff…").grab_focus();
                            }
                            // The focus ring eases in: capture after it.
                            waited.set(waited.get() + 1);
                            if waited.get() >= 6 && shot(&window, "present-focus") {
                                waited.set(0);
                                assert!(open_pickup(&root));
                                step.set(Step::Dialog(0));
                            }
                        }
                    }
                    Step::Dialog(round) => {
                        let Some(d) = window.visible_dialog() else {
                            return glib::ControlFlow::Continue;
                        };
                        if !d.is_mapped() || d.width() <= 0 {
                            return glib::ControlFlow::Continue;
                        }
                        let d = d.downcast::<adw::AlertDialog>().unwrap();
                        // A dialog already answered can linger as the
                        // window's visible dialog; wait for the new one.
                        if handled.borrow().as_ref() == Some(&d) {
                            return glib::ControlFlow::Continue;
                        }
                        // Every round waits for the opening to settle; a
                        // response during the transition is not delivered.
                        // The content must map in every opening (CC7b: it
                        // did not when focus was left on a hidden control).
                        waited.set(waited.get() + 1);
                        let Some(confirm) = mapped_button(d.upcast_ref(), Action::Pickup.label())
                        else {
                            return glib::ControlFlow::Continue;
                        };
                        if waited.get() < 5 || (round == 0 && !shot(&window, "pickup-dialog")) {
                            return glib::ControlFlow::Continue;
                        }
                        println!("DIALOG {round} content mapped after {} ticks", waited.get());
                        waited.set(0);
                        assert_eq!(d.default_response().as_deref(), Some("cancel"));
                        assert!(d.body().contains("same pane"));
                        if round == 0 {
                            println!("TABORDER dialog: {}", tab_order(&window).join(" > "));
                        }
                        d.connect_response(None, move |_, r| println!("DIALOG {round} {r}"));
                        assert!(confirm.grab_focus() && confirm.activate());
                        handled.replace(Some(d.clone()));
                        step.set(match round {
                            0 => Step::Stage(0),
                            1 => Step::Failure,
                            _ => Step::Unknown,
                        });
                    }
                    Step::Stage(i) => {
                        let want = if i == 5 {
                            StageMark::Loaded
                        } else {
                            StageMark::Now
                        };
                        if marks.is_some_and(|m| m[i] == want) {
                            let name: &'static str = Box::leak(
                                format!("stage-{}-{:?}", i + 1, STAGES[i]).into_boxed_str(),
                            );
                            if shot(&window, name) {
                                println!("STAGE {} {}", i + 1, labels(&root).replace('\n', " | "));
                                GATE.store(i + 1, Ordering::SeqCst);
                                step.set(if i == 5 {
                                    Step::Wait(1)
                                } else {
                                    Step::Stage(i + 1)
                                });
                            }
                        }
                    }
                    Step::Wait(round) => {
                        // Rediscovery after the terminal result, then a new pickup.
                        let key = if round == 1 {
                            "scenario-fail-2"
                        } else {
                            "scenario-unknown-4"
                        };
                        GATE.store(9, Ordering::SeqCst);
                        // Same key again is a no-op: no rediscovery per tick.
                        controls.set_session(Some(&native("pi", key)));
                        if button(&root, PICKUP).is_visible() {
                            // CC7b: the controls that left during the last
                            // pickup must not leave focus on a hidden widget.
                            let focus = gtk::prelude::GtkWindowExt::focus(&window);
                            assert!(
                                focus
                                    .as_ref()
                                    .is_some_and(|f| f.is_visible() && f.is_drawable()),
                                "focus before dialog {round} on a hidden widget: {:?}",
                                focus.map(|f| (f.type_().name(), f.is_visible()))
                            );
                            println!("FOCUS round {round}: {}", focus.unwrap().type_().name());
                            assert!(open_pickup(&root));
                            step.set(Step::Dialog(round));
                        }
                    }
                    Step::Failure => {
                        if marks.is_some_and(|m| m[2] == StageMark::Stopped)
                            && shot(&window, "failure-stage-3-Clearing")
                        {
                            println!("FAILURE {}", labels(&root).replace('\n', " | "));
                            step.set(Step::Wait(2));
                        }
                    }
                    Step::Unknown => {
                        if marks.is_some_and(|m| m[4] == StageMark::Unknown) {
                            let order = tab_order(&window);
                            assert!(order.iter().any(|o| o.starts_with("Query operation")));
                            window.set_focus_visible(true);
                            button_starting(&root, "Query operation").grab_focus();
                            if shot(&window, "unknown-stage-5-PickupSent") {
                                println!("TABORDER unknown: {}", order.join(" > "));
                                println!("UNKNOWN {}", labels(&root).replace('\n', " | "));
                                println!("ANNOUNCED {}", controls.announcements().join(" || "));
                                window.application().unwrap().quit();
                                return glib::ControlFlow::Break;
                            }
                        }
                    }
                }
                glib::ControlFlow::Continue
            });
        });
        app.run_with_args::<&str>(&[]);
    }

    /// The alert dialog keeps one button per layout; activate the shown one.
    fn mapped_button(root: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(b) = root.downcast_ref::<gtk::Button>()
            && b.label().as_deref() == Some(label)
            && b.is_mapped()
        {
            return Some(b.clone());
        }
        let mut child = root.first_child();
        while let Some(w) = child {
            if let Some(b) = mapped_button(&w, label) {
                return Some(b);
            }
            child = w.next_sibling();
        }
        None
    }
    fn button_starting(root: &gtk::Box, prefix: &str) -> gtk::Button {
        let mut child = root.first_child();
        while let Some(w) = child {
            if let Some(b) = w.downcast_ref::<gtk::Button>()
                && b.label().is_some_and(|l| l.starts_with(prefix))
            {
                return b.clone();
            }
            child = w.next_sibling();
        }
        panic!("missing button {prefix}");
    }
    /// Visible control buttons directly under the section.
    fn session_control_buttons(root: &gtk::Box) -> Vec<String> {
        let mut out = Vec::new();
        let mut child = root.first_child();
        while let Some(w) = child {
            if let Some(b) = w.downcast_ref::<gtk::Button>()
                && w.is_visible()
            {
                out.push(b.label().unwrap_or_default().to_string());
            }
            child = w.next_sibling();
        }
        out
    }
}

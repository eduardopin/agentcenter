//! GTK presentation/scheduling regressions with injected controller state.
//! These tests do not exercise a socket, Pi, SDK, or persisted effects.
use super::*;

fn selected(identity: &str) -> Session {
    Session {
        agent: Agent::Pi,
        identity: identity.into(),
        revision: "r1".into(),
        name: identity.into(),
        handoff_destination: "/private".into(),
        expected: serde_json::json!({"managerId":"m", "revision":"1", "sessionId":"s", "sessionFile":"/session.jsonl", "leafId":"old"}),
        handoff_disabled: None,
        compact_disabled: None,
        pickup_disabled: None,
        cancel_supported: true,
    }
}

fn running_panel() -> Rc<Panel> {
    let p = SessionControls::new(false).panel;
    let s = selected("session-a");
    let mut c = p.controller.borrow_mut();
    c.select(Some(s.clone()));
    let prompt = c.prepare(Action::Handoff).unwrap();
    c.confirm(prompt, &s, "op-a".into()).unwrap();
    assert!(c.update("op-a", "session-a", Status::Running));
    drop(c);
    p.render();
    p
}

/// Polling's busy flag must not hide cancel or consume its queued request.
#[test]
fn gtk_busy_poll_keeps_cancel_available_and_duplicate_click_queued_once() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let p = running_panel();
        p.busy.set(true);
        p.render();
        assert!(p.cancel.is_visible() && p.cancel.is_sensitive());
        // Running is polled by itself: no Query, and no action buttons.
        assert!(!p.query.is_visible());
        assert!(p.buttons.iter().all(|(_, b)| !b.is_visible()));
        p.cancel.emit_clicked();
        assert!(p.cancel_pending.get());
        assert!(p.controller.borrow().cancel_requested);
        // Requested once: the control leaves instead of staying dead.
        assert!(!p.cancel.is_visible());
        assert!(p.reason.text().contains("waiting for acknowledgement"));
        // emit_clicked deliberately bypasses insensitive pointer filtering.
        p.cancel.emit_clicked();
        assert!(p.cancel_pending.get());
        assert_eq!(p.controller.borrow().query().unwrap().operation_id, "op-a");
        let weak = Rc::downgrade(&p);
        let dispatched = Rc::new(Cell::new(false));
        let done = dispatched.clone();
        glib::idle_add_local_once(move || {
            let p = weak.upgrade().unwrap();
            p.reconcile(agentcenter_core::control::pi::Action::Cancel);
            assert!(p.cancel_pending.get(), "busy reconcile lost queued cancel");
            assert!(p.busy.get());
            done.set(true);
        });
        let context = glib::MainContext::default();
        while !dispatched.get() {
            context.iteration(true);
        }
        assert_eq!(
            p.controller.borrow().visible_status(),
            Some(&Status::Running)
        );
    });
}

/// An old session's late acknowledgement must never render on the new target.
#[test]
fn gtk_selection_change_hides_late_ack_and_keeps_cancel_target_explicit() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let p = running_panel();
        p.controller
            .borrow_mut()
            .select(Some(selected("session-b")));
        p.render();
        assert!(p.cancel.label().unwrap().contains("session-a"));
        assert!(!p.reason.text().contains("Running"));
        assert!(
            p.reason
                .text()
                .contains("Unresolved operation for session-a")
        );
        assert!(p.buttons.iter().all(|(_, b)| !b.is_visible()));
        assert!(
            p.controller
                .borrow_mut()
                .update("op-a", "session-a", Status::Cancelled)
        );
        p.render();
        assert_eq!(p.controller.borrow().visible_status(), None);
        assert!(!p.reason.text().contains("Cancelled"));
        assert!(
            !p.reason.is_visible(),
            "full capability needs no reason line"
        );
        assert!(!p.cancel.is_visible());
        assert!(
            p.buttons
                .iter()
                .all(|(_, b)| b.is_visible() && b.is_sensitive())
        );
    });
}

#[test]
fn gtk_terminal_result_preserves_capability_reasons_and_handoff_discloses_provider() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let p = running_panel();
        let original = p.controller.borrow().selection.clone().unwrap();
        let prompt = Confirmation {
            session: original.clone(),
            action: Action::Handoff,
            generation: 0,
        };
        let body = prompt.body();
        assert!(
            body.contains("Sends projected session text to the provider currently selected in Pi")
        );
        assert!(body.contains("may consume tokens"));
        assert!(body.contains("Does not clear"));
        assert!(p.controller.borrow_mut().update(
            "op-a",
            "session-a",
            Status::Failed("quota".into())
        ));
        let mut current = original;
        current.handoff_disabled = Some("model_auth_required".into());
        current.compact_disabled = Some("busy".into());
        p.controller.borrow_mut().select(Some(current));
        p.render();
        assert!(p.reason.text().contains("Failed: quota"));
        assert!(p.buttons[..2].iter().all(|(_, b)| !b.is_visible()));
    });
}

/// Duplicate confirmation and an old operation's ack cannot replace a newer operation.
#[test]
fn gtk_duplicate_confirmation_and_old_ack_leave_new_operation_untouched() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let p = running_panel();
        let next = selected("session-b");
        let mut c = p.controller.borrow_mut();
        assert!(c.update("op-a", "session-a", Status::Cancelled));
        c.select(Some(next.clone()));
        let prompt = c.prepare(Action::Compact).unwrap();
        c.confirm(prompt.clone(), &next, "op-b".into()).unwrap();
        assert!(c.confirm(prompt, &next, "duplicate".into()).is_err());
        assert!(!c.update("op-a", "session-a", Status::Running));
        assert_eq!(c.query().unwrap().operation_id, "op-b");
        drop(c);
        p.render();
        assert!(p.reason.text().starts_with(&Status::Requested.text()));
        assert!(p.cancel.label().unwrap().contains("session-b"));
        assert!(p.buttons.iter().all(|(_, b)| !b.is_visible()));
    });
}

/// Owner rule (22/10): no dead controls. Without a controllable session the
/// panel shows only the reason; query/cancel appear only for an open operation.
/// Focus follows whatever leaves the section (fixture round 3 regression):
/// the status line that held it hides when the actions return, a focused
/// action leaves when an operation starts, and with nothing left to show the
/// section itself holds focus. Never an invisible widget.
#[test]
fn gtk_focus_never_stays_on_a_hidden_control_or_status_line() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let p = running_panel();
        let window = gtk::Window::new();
        window.set_child(Some(&p.widget));
        window.present();
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !p.widget.is_mapped() && std::time::Instant::now() < deadline {
            context.iteration(false);
        }
        let focus = || gtk::prelude::GtkWindowExt::focus(&window).expect("focus in the window");
        let is = |w: &gtk::Widget, want: &gtk::Widget| {
            assert!(
                w == want && w.is_visible(),
                "focus on {} (visible={}), want {}",
                w.type_().name(),
                w.is_visible(),
                want.type_().name()
            );
        };
        p.controller
            .borrow_mut()
            .select(Some(selected("session-b")));
        p.render();
        assert!(p.reason.is_visible() && p.reason.grab_focus());
        // The operation ends: the line that held focus hides, actions return.
        assert!(
            p.controller
                .borrow_mut()
                .update("op-a", "session-a", Status::Cancelled)
        );
        p.render();
        assert!(!p.reason.is_visible());
        is(&focus(), p.buttons[0].1.upcast_ref());
        // The focused action leaves when an operation starts: on to Cancel.
        let s = selected("session-b");
        {
            let mut c = p.controller.borrow_mut();
            let prompt = c.prepare(Action::Handoff).unwrap();
            c.confirm(prompt, &s, "op-b".into()).unwrap();
            assert!(c.update("op-b", "session-b", Status::Running));
        }
        p.render();
        assert!(p.buttons.iter().all(|(_, b)| !b.is_visible()));
        is(&focus(), p.cancel.upcast_ref());
        // Cancel requested: it leaves too, the status line takes focus.
        assert!(p.controller.borrow_mut().request_cancel().is_some());
        p.render();
        assert!(!p.cancel.is_visible());
        is(&focus(), p.reason.upcast_ref());
        assert_eq!(p.reason.selection_bounds(), None, "status line highlighted");
        // Nothing left to show: the section itself, then never a Tab stop
        // once focus moves on.
        p.reason.set_visible(false);
        p.keep_focus();
        is(&focus(), p.widget.upcast_ref());
        // A control coming back takes focus from the section, which stops
        // being a Tab stop.
        assert!(
            p.controller
                .borrow_mut()
                .update("op-b", "session-b", Status::Unknown)
        );
        p.render();
        is(&focus(), p.query.upcast_ref());
        assert!(!p.widget.is_focusable(), "section stays a Tab stop");
    });
}

/// Handoff and Compact are announced to the screen reader like the pickup:
/// once per status (sent, saved/compacted, failure), never once per render.
#[test]
fn gtk_handoff_and_compact_status_is_announced_once_each() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let p = running_panel();
        p.render();
        p.render();
        let saved = Status::Completed(Evidence::Persisted {
            path: "/private/h.md".into(),
            size: 12,
            sha256: SHA.into(),
        });
        assert_eq!(
            p.announcements.borrow().clone(),
            ["Handoff: Running — completion not confirmed"]
        );
        {
            let mut c = p.controller.borrow_mut();
            c.operation.as_mut().unwrap().1 = saved.clone();
        }
        p.render();
        p.render();
        // Same operation seen again from another selection and back: silent.
        let s = p.controller.borrow().selection.clone();
        p.controller.borrow_mut().select(Some(selected("other")));
        p.render();
        p.controller.borrow_mut().select(s.clone());
        p.render();
        assert_eq!(
            p.announcements.borrow().clone(),
            [
                "Handoff: Running — completion not confirmed".to_owned(),
                format!("Handoff: {}", saved.text()),
            ]
        );
        // A compact that is sent to the pane and then fails.
        let s = s.unwrap();
        {
            let mut c = p.controller.borrow_mut();
            let prompt = c.prepare(Action::Compact).unwrap();
            c.confirm(prompt, &s, "op-c".into()).unwrap();
        }
        p.render();
        for status in [Status::Sent, Status::Failed("quota".into())] {
            assert!(
                p.controller
                    .borrow_mut()
                    .update("op-c", "session-a", status)
            );
            p.render();
            p.render();
        }
        let log = p.announcements.borrow().clone();
        assert_eq!(
            &log[2..],
            [
                format!("Compact: {}", Status::Sent.text()),
                "Compact: Failed: quota".to_owned(),
            ]
        );
    });
}

#[test]
fn gtk_unsupported_selection_hides_every_control() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let controls = SessionControls::new(true);
        let p = &controls.panel;
        controls.set_session(None);
        assert!(p.buttons.iter().all(|(_, b)| !b.is_visible()));
        assert!(!p.query.is_visible());
        assert!(!p.cancel.is_visible());
        assert!(p.reason.is_visible());

        let running = running_panel();
        // In flight, the actions give way to the operation's own controls.
        assert!(running.buttons.iter().all(|(_, b)| !b.is_visible()));
        assert!(running.cancel.is_visible());
        assert!(
            running
                .controller
                .borrow_mut()
                .update("op-a", "session-a", Status::Unknown)
        );
        running.render();
        assert!(running.query.is_visible() && running.query.is_sensitive());
        // Moving to an unsupported selection keeps the open operation reachable.
        running.controller.borrow_mut().select(None);
        running.render();
        assert!(running.buttons.iter().all(|(_, b)| !b.is_visible()));
        assert!(running.query.is_visible());
        assert!(
            running
                .reason
                .text()
                .contains("Unresolved operation for session-a")
        );
    });
}

static AUTHENTICATIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Counts discovery + authentication without touching a real runtime directory.
fn counting_authenticate(
    _: &agentcenter_core::Session,
) -> Result<(Backend, Session), &'static str> {
    AUTHENTICATIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Err("synthetic: no extension")
}

fn authentications() -> usize {
    AUTHENTICATIONS.load(std::sync::atomic::Ordering::SeqCst)
}

/// A sampled Pi session from the typical fixture; `tick` changes only metrics.
fn sampled_pi(session_id: &str, tick: u32) -> agentcenter_core::Session {
    let snap: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/snapshots/tipico-40.json"
    ))
    .unwrap();
    let mut s = snap["sessions"][0].clone();
    s["agent"] = "pi".into();
    s["starttime"] = 4242.into();
    s["overlay"]["agent_session_id"] = session_id.into();
    s["cpu_pct"] = (f64::from(tick) / 10.0).into();
    s["procs"] = (tick + 1).into();
    serde_json::from_value(s).unwrap()
}

/// Runs queued futures until the panel has no discovery in flight.
fn settle(p: &Panel) {
    let context = glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while (p.busy.get() || context.pending()) && std::time::Instant::now() < deadline {
        context.iteration(false);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(!p.busy.get(), "discovery did not settle");
}

/// Detail calls `set_session` on every 1 Hz tick. A stable selection must not
/// rediscover or authenticate again, and nothing may poll without an operation.
#[test]
fn gtk_stable_selection_ticks_do_not_rediscover_or_poll() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let controls = SessionControls::new(true);
        let p = &controls.panel;
        p.authenticate.set(counting_authenticate);
        let before = authentications();
        controls.set_session(Some(&sampled_pi("native-a", 0)));
        settle(p);
        assert_eq!(authentications() - before, 1, "selection discovers once");
        let epoch = p.epoch.get();
        for tick in 1..=60 {
            controls.set_session(Some(&sampled_pi("native-a", tick)));
            settle(p);
        }
        assert_eq!(authentications() - before, 1, "ticks rediscovered");
        assert_eq!(p.epoch.get(), epoch);
        assert!(p.operation.borrow().is_none());
        assert!(!p.refresh_pending.get());

        // A different native identity is a new selection: exactly one more.
        controls.set_session(Some(&sampled_pi("native-b", 61)));
        settle(p);
        assert_eq!(authentications() - before, 2);

        // Recorded snapshot (fixture mode): never discovers, on any tick.
        let recorded = SessionControls::new(false);
        recorded.panel.authenticate.set(counting_authenticate);
        for tick in 0..60 {
            recorded.set_session(Some(&sampled_pi("native-a", tick)));
            settle(&recorded.panel);
        }
        assert_eq!(authentications() - before, 2, "fixture mode discovered");
    });
}

/// Synthetic discovery for the capability matrix. Keyed by the sampled
/// native session ID; never reaches a socket, /proc or a transcript.
fn matrix_discovery(
    native: &agentcenter_core::Session,
) -> Result<(Backend, Session), &'static str> {
    assert!(
        !glib::MainContext::default().is_owner(),
        "discovery ran on the GTK thread"
    );
    let key = native
        .overlay
        .as_ref()
        .unwrap()
        .agent_session_id
        .clone()
        .unwrap();
    let mut s = selected(&key);
    s.cancel_supported = false;
    s.agent = if native.agent == "claude" {
        Agent::Claude
    } else {
        Agent::Pi
    };
    match key.as_str() {
        "pi-no-extension" => return Err("No matching extension; enable it manually in Pi"),
        "pi-busy" | "claude-busy" => {
            s.handoff_disabled = Some("busy".into());
            s.compact_disabled = Some("busy".into());
            s.pickup_disabled = Some("busy".into());
        }
        "pi-outside-herdr" => s.pickup_disabled = Some(reason::PI_OUTSIDE_HERDR.into()),
        _ => {}
    }
    // The pane says the agent is working: a real capability change.
    let pane = native.overlay.as_ref().unwrap().herdr.as_ref();
    if pane.and_then(|h| h.status.as_deref()) == Some("working") {
        s.handoff_disabled = Some("busy".into());
        s.compact_disabled = Some("busy".into());
        s.pickup_disabled = Some("busy".into());
    }
    let herdr = agentcenter_core::control::herdr::Endpoint {
        socket: "/nonexistent".into(),
        server_pid: 0,
        server_starttime: 0,
    };
    Ok((Backend::Claude(herdr), s))
}

fn sampled(agent: &str, key: &str, herdr: bool) -> agentcenter_core::Session {
    let mut s = sampled_pi(key, 0);
    s.agent = agent.into();
    let overlay = s.overlay.as_mut().unwrap();
    overlay.herdr = herdr.then(|| agentcenter_core::overlay::HerdrOverlay {
        workspace_id: Some("w1".into()),
        tab_id: Some("t1".into()),
        pane_id: Some("p1".into()),
        title: None,
        status: Some("idle".into()),
    });
    if agent == "claude" {
        overlay.agent_session_id = Some(format!("{key}-0000-4000-8000-000000000000"));
    }
    s
}

/// Every visible button under the section, in tree order.
fn visible_buttons(root: &gtk::Widget) -> Vec<String> {
    let mut out = Vec::new();
    let mut child = root.first_child();
    while let Some(w) = child {
        if w.is_visible() {
            if let Some(b) = w.downcast_ref::<gtk::Button>() {
                out.push(b.label().unwrap_or_default().to_string());
            }
            out.extend(visible_buttons(&w));
        }
        child = w.next_sibling();
    }
    out
}

/// Acceptance 1 (CC7): the section follows real capability only. Without
/// it there is no button in the widget tree, and at most one reason line.
#[test]
fn gtk_capability_matrix_shows_only_runnable_controls() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let all = ["Handoff…", "Compact…", "Handoff + new session…"];
        type Case<'a> = (&'a str, &'a str, bool, &'a [&'a str], Option<&'a str>);
        let cases: [Case; 9] = [
            ("pi", "pi-idle-herdr", true, &all, None),
            (
                "pi",
                "pi-outside-herdr",
                false,
                &all[..2],
                Some("needs Pi inside a herdr pane"),
            ),
            (
                "pi",
                "pi-no-extension",
                false,
                &[],
                Some("extension not found"),
            ),
            ("pi", "pi-busy", true, &[], Some("Pi is busy")),
            ("claude", "c1d1e1f1", true, &all, None),
            ("claude", "c2b2b2b2", true, &[], Some("Claude Code is busy")),
            ("claude", "c3o3o3o3", false, &[], Some("outside herdr")),
            ("codex", "codex-1", true, &[], Some("no control channel")),
            ("gemini", "gemini-1", false, &[], Some("no control channel")),
        ];
        for (agent, key, herdr, expected, why) in cases {
            let controls = SessionControls::new(true);
            controls.set_discovery(matrix_discovery);
            // The busy Claude fixture is keyed by its prefix below.
            let mut native = sampled(agent, key, herdr);
            if key == "c2b2b2b2" {
                native.overlay.as_mut().unwrap().agent_session_id = Some("claude-busy".into());
            }
            controls.set_session(Some(&native));
            settle(&controls.panel);
            let shown = visible_buttons(controls.widget.upcast_ref());
            assert_eq!(shown, expected, "{agent}/{key}");
            let p = &controls.panel;
            match why {
                None => assert!(!p.reason.is_visible(), "{key}: {}", p.reason.text()),
                Some(fragment) => {
                    assert!(p.reason.is_visible(), "{key}");
                    let text = p.reason.text();
                    assert!(text.contains(fragment), "{key}: {text}");
                    assert!(!text.contains('\n'), "{key}: more than one line: {text}");
                    // Accessible text, not a tooltip-only explanation.
                    assert!(p.reason.tooltip_text().is_none());
                }
            }
        }
    });
}

/// Every visible label under the section with text, in tree order.
fn visible_lines(root: &gtk::Widget) -> Vec<String> {
    let mut out = Vec::new();
    let mut child = root.first_child();
    while let Some(w) = child {
        if w.is_visible() && w.downcast_ref::<gtk::Button>().is_none() {
            if let Some(l) = w.downcast_ref::<gtk::Label>()
                && !l.text().is_empty()
            {
                out.push(l.text().to_string());
            }
            out.extend(visible_lines(&w));
        }
        child = w.next_sibling();
    }
    out
}

/// B1 (verifier, CC7b): ONE panel walks the matrix in sequence, as the detail
/// pane does when the owner moves down the board. Leaving a controllable
/// session for one without control must show its reason, never an empty
/// section: zero buttons and exactly one visible line.
#[test]
fn gtk_same_panel_keeps_the_reason_when_moving_to_a_session_without_control() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let controls = SessionControls::new(true);
        controls.set_discovery(matrix_discovery);
        let idle = sampled("pi", "pi-idle-herdr", true);
        let steps: [(Option<agentcenter_core::Session>, Option<&str>); 9] = [
            (Some(idle.clone()), None),
            (
                Some(sampled("codex", "codex-1", true)),
                Some("no control channel"),
            ),
            (Some(idle.clone()), None),
            (
                Some(sampled("claude", "c3o3o3o3", false)),
                Some("outside herdr"),
            ),
            (Some(idle.clone()), None),
            (None, Some("Select a session")),
            (Some(idle.clone()), None),
            (
                Some(sampled("pi", "pi-no-extension", false)),
                Some("extension not found"),
            ),
            (
                Some(sampled("gemini", "gemini-1", false)),
                Some("no control channel"),
            ),
        ];
        for (i, (session, why)) in steps.iter().enumerate() {
            controls.set_session(session.as_ref());
            settle(&controls.panel);
            let root = controls.widget.upcast_ref();
            let lines = visible_lines(root);
            match why {
                None => {
                    assert_eq!(visible_buttons(root).len(), 3, "step {i}");
                    assert!(lines.is_empty(), "step {i}: {lines:?}");
                }
                Some(fragment) => {
                    assert!(visible_buttons(root).is_empty(), "step {i}");
                    assert_eq!(lines.len(), 1, "step {i}: {lines:?}");
                    assert!(lines[0].contains(fragment), "step {i}: {lines:?}");
                    assert!(!lines[0].contains('\n'), "step {i}: {lines:?}");
                }
            }
        }
    });
}

/// B2 (verifier, CC7b): rediscovery of the session already shown (window
/// focus back, herdr status change) keeps its controls in place and the
/// keyboard focus where it was; only a real capability change moves them.
#[test]
fn gtk_rediscovery_keeps_controls_and_focus_until_capability_changes() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let controls = SessionControls::new(true);
        controls.set_discovery(matrix_discovery);
        let p = controls.panel.clone();
        let window = gtk::Window::new();
        window.set_child(Some(&controls.widget));
        window.present();
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !p.widget.is_mapped() && std::time::Instant::now() < deadline {
            context.iteration(false);
        }
        let mut native = sampled("pi", "pi-idle-herdr", true);
        controls.set_session(Some(&native));
        settle(&p);
        let compact = p.buttons[1].1.clone();
        assert!(compact.grab_focus());
        let focus = || gtk::prelude::GtkWindowExt::focus(&window);
        let all = ["Handoff…", "Compact…", "Handoff + new session…"];
        let unchanged = |when: &str| {
            assert_eq!(visible_buttons(controls.widget.upcast_ref()), all, "{when}");
            assert!(!p.reason.is_visible(), "{when}: {}", p.reason.text());
            assert_eq!(
                focus().as_ref(),
                Some(compact.upcast_ref::<gtk::Widget>()),
                "{when}: focus moved"
            );
        };
        // Pane status changes without changing capability.
        for status in ["done", "idle"] {
            native
                .overlay
                .as_mut()
                .unwrap()
                .herdr
                .as_mut()
                .unwrap()
                .status = Some(status.into());
            controls.set_session(Some(&native));
            assert!(p.busy.get(), "{status}: no rediscovery");
            unchanged(&format!("{status}, in flight"));
            settle(&p);
            unchanged(&format!("{status}, settled"));
        }
        // The window gets focus back.
        p.rediscover();
        assert!(p.busy.get());
        unchanged("focus back, in flight");
        settle(&p);
        unchanged("focus back, settled");
        // A click during rediscovery is never dead: the dialog opens.
        p.rediscover();
        assert!(p.busy.get());
        let before = gtk::Window::list_toplevels().len();
        compact.emit_clicked();
        let opened = gtk::Window::list_toplevels();
        assert_eq!(
            opened.len(),
            before + 1,
            "click during rediscovery was dead"
        );
        settle(&p);
        for w in opened {
            if let Some(w) = w.downcast_ref::<gtk::Window>()
                && w != &window
            {
                w.close();
            }
        }
        settle(&p);
        // Capability really drops: controls leave, focus stays visible.
        native
            .overlay
            .as_mut()
            .unwrap()
            .herdr
            .as_mut()
            .unwrap()
            .status = Some("working".into());
        controls.set_session(Some(&native));
        settle(&p);
        assert!(visible_buttons(controls.widget.upcast_ref()).is_empty());
        assert_eq!(
            p.reason.text(),
            "Pi is busy; controls return when it is idle."
        );
        let f = focus().expect("focus in the window");
        assert!(f.is_visible(), "focus on a hidden {}", f.type_().name());
    });
}

/// Re-parenting the section (unrealize/realize) must not stack window
/// activation handlers: one activation, one rediscovery.
#[test]
fn gtk_reparenting_keeps_one_activation_handler() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let controls = SessionControls::new(true);
        let p = controls.panel.clone();
        p.authenticate.set(counting_authenticate);
        let window = gtk::Window::new();
        let context = glib::MainContext::default();
        for _ in 0..4 {
            window.set_child(Some(&controls.widget));
            window.present();
            while !controls.widget.is_realized() {
                context.iteration(false);
            }
            window.set_child(None::<&gtk::Widget>);
            assert!(p.active.borrow().is_none(), "handler kept after unrealize");
        }
        window.set_child(Some(&controls.widget));
        while !controls.widget.is_realized() {
            context.iteration(false);
        }
        controls.set_session(Some(&sampled_pi("native-r", 0)));
        settle(&p);
        let before = authentications();
        // Count the handlers GTK holds for this window's is-active notify.
        let (_, id) = p.active.borrow_mut().take().expect("one handler");
        window.disconnect(id);
        assert!(p.active.borrow().is_none());
        // With ours removed, no stale handler of this panel is left to fire.
        assert!(window.is_active(), "Broadway window not active");
        window.notify("is-active");
        settle(&p);
        assert_eq!(authentications(), before, "a stacked handler fired");
    });
}

/// A capability that drops while the window is open hides the button on the
/// next rediscovery (herdr status change), never leaves it insensitive.
#[test]
fn gtk_capability_drop_hides_instead_of_disabling() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let controls = SessionControls::new(true);
        controls.set_discovery(matrix_discovery);
        let mut native = sampled("pi", "pi-idle-herdr", true);
        controls.set_session(Some(&native));
        settle(&controls.panel);
        assert_eq!(visible_buttons(controls.widget.upcast_ref()).len(), 3);
        native.overlay.as_mut().unwrap().agent_session_id = Some("pi-busy".into());
        // Same process, new native ID: a new selection, rediscovered once.
        controls.set_session(Some(&native));
        settle(&controls.panel);
        assert!(visible_buttons(controls.widget.upcast_ref()).is_empty());
        let p = &controls.panel;
        assert!(p.buttons.iter().all(|(_, b)| !b.is_visible()));
        assert_eq!(
            p.reason.text(),
            "Pi is busy; controls return when it is idle."
        );
    });
}

// ---- CC7 pickup: stages, announcements and injected failures ----

use agentcenter_core::control::{Artifact, State, pickup::OldSession};

const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// One progress report for `request` at `stage`, as an adapter would send it.
fn progress(
    request: &Request,
    stage: Stage,
    state: State,
    new_session_id: Option<&str>,
) -> Progress {
    Progress {
        operation_id: request.operation_id.clone(),
        state,
        stage,
        code: "injected",
        old_session: OldSession {
            session_id: request.session.expected["sessionId"]
                .as_str()
                .unwrap()
                .into(),
            session_file: request.session.expected["sessionFile"]
                .as_str()
                .unwrap_or("/claude.jsonl")
                .into(),
        },
        handoff: (stage >= Stage::HandoffPersisted).then(|| Artifact {
            path: format!("/private/nonce-{}.md", request.operation_id).into(),
            size: 2048,
            sha256: SHA.into(),
            modified_ms: 1_760_000_000_000,
        }),
        new_session_id: new_session_id.map(Into::into),
    }
}

/// Scripted pickup adapter keyed by the session identity:
/// `pickup-ok`, `pickup-fail-<n>` or `pickup-unknown-<n>` (n = stage index).
struct ScriptedPort {
    request: Request,
    reads: usize,
}
static PICKUP_STARTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
impl ScriptedPort {
    fn at(&self, index: usize) -> Progress {
        let key = self.request.session.identity.as_str();
        let stop = key.rsplit('-').next().and_then(|n| n.parse::<usize>().ok());
        let stage = STAGES[index.min(stop.unwrap_or(5))];
        let state = match (key.contains("-fail-"), key.contains("-unknown-"), stop) {
            (true, _, Some(n)) if index >= n => State::Failed,
            (_, true, Some(n)) if index >= n => State::Unknown,
            _ if index >= 5 => State::Completed,
            _ if index == 0 => State::Accepted,
            _ => State::Running,
        };
        let new = (state == State::Completed).then_some("new-session-7");
        let mut p = progress(&self.request, stage, state, new);
        if state == State::Failed {
            p.code = "injected_failure";
        }
        p
    }
}
impl PickupPort for ScriptedPort {
    fn start(&mut self, operation_id: &str) -> Result<Progress, &'static str> {
        assert!(
            !glib::MainContext::default().is_owner(),
            "pickup start ran on the GTK thread"
        );
        assert_eq!(operation_id, self.request.operation_id);
        PICKUP_STARTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.at(0))
    }
    fn status(&mut self, operation_id: &str) -> Result<Progress, &'static str> {
        assert_eq!(operation_id, self.request.operation_id, "status changed id");
        self.reads += 1;
        Ok(self.at(self.reads))
    }
}
fn scripted_connect(
    _: &Backend,
    _: &agentcenter_core::Session,
    request: &Request,
) -> Result<Box<dyn PickupPort>, &'static str> {
    Ok(Box::new(ScriptedPort {
        request: request.clone(),
        reads: 0,
    }))
}
/// Discovery for pickup cases: full capability, identity = native key.
fn pickup_discovery(
    native: &agentcenter_core::Session,
) -> Result<(Backend, Session), &'static str> {
    let key = native
        .overlay
        .as_ref()
        .unwrap()
        .agent_session_id
        .clone()
        .unwrap();
    let mut s = selected(&key);
    s.cancel_supported = true;
    if native.agent == "claude" {
        s.agent = Agent::Claude;
        s.expected = serde_json::json!({"sessionId": "old-claude-id"});
    }
    let herdr = agentcenter_core::control::herdr::Endpoint {
        socket: "/nonexistent".into(),
        server_pid: 0,
        server_starttime: 0,
    };
    Ok((Backend::Claude(herdr), s))
}

/// Selects `key`, confirms the pickup and lets the scripted adapter run.
fn run_pickup(agent: &str, key: &str) -> SessionControls {
    let controls = SessionControls::new(true);
    controls.set_discovery(pickup_discovery);
    controls.set_pickup(scripted_connect);
    controls.set_poll(std::time::Duration::from_millis(1));
    let mut native = sampled(agent, key, true);
    native.overlay.as_mut().unwrap().agent_session_id = Some(key.into());
    controls.set_session(Some(&native));
    settle(&controls.panel);
    assert!(
        visible_buttons(controls.widget.upcast_ref()).contains(&"Handoff + new session…".into()),
        "{key}: pickup offered"
    );
    let prompt = controls
        .panel
        .controller
        .borrow()
        .prepare(Action::Pickup)
        .unwrap();
    controls.panel.dispatch(prompt);
    settle(&controls.panel);
    controls
}

fn check_visible(p: &Panel) -> Vec<bool> {
    p.stages.rows.iter().map(|r| r.check.is_visible()).collect()
}

/// Every stage, injected directly: amber NOW on the current one, done before,
/// pending after, one announcement per change, no success mark until loaded.
#[test]
fn gtk_each_pickup_stage_is_shown_and_announced_once() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let controls = SessionControls::new(false);
        let p = &controls.panel;
        let s = selected("session-a");
        let request = {
            let mut c = p.controller.borrow_mut();
            c.select(Some(s.clone()));
            let prompt = c.prepare(Action::Pickup).unwrap();
            c.confirm(prompt, &s, "op-p".into()).unwrap()
        };
        p.render();
        assert!(!p.stages.widget.is_visible(), "no stage before any report");
        for (i, stage) in STAGES.into_iter().enumerate() {
            let report = progress(&request, stage, State::Running, None);
            let status = status_from_progress(&request, &report);
            assert!(
                p.controller
                    .borrow_mut()
                    .update("op-p", "session-a", status)
            );
            p.apply_progress(&request, Some(report));
            p.render();
            p.render(); // A second render must not announce again.
            let marks = controls.stage_marks().unwrap();
            for (j, mark) in marks.iter().enumerate() {
                let want = match j.cmp(&i) {
                    std::cmp::Ordering::Less => StageMark::Done,
                    std::cmp::Ordering::Equal => StageMark::Now,
                    std::cmp::Ordering::Greater => StageMark::Pending,
                };
                assert_eq!(*mark, want, "stage {i}, row {j}");
            }
            let row = &p.stages.rows[i].state;
            assert!(row.has_css_class("busy") && row.has_css_class("ac-pill"));
            assert!(!row.has_css_class("blocked"));
            assert!(check_visible(p).iter().all(|v| !v), "check before evidence");
            assert!(
                p.stages
                    .title
                    .text()
                    .contains(&format!("stage {} of 6", i + 1))
            );
            assert!(p.reason.text().contains(stage_name(stage)));
            let said = controls.announcements();
            assert_eq!(said.len(), i + 1, "one announcement per stage: {said:?}");
            assert!(said[i].contains(stage_name(stage)), "{}", said[i]);
            // Pickup has no cancel in its contract.
            assert!(!p.cancel.is_visible());
        }
        let done = progress(
            &request,
            Stage::PickupLoaded,
            State::Completed,
            Some("new-7"),
        );
        let status = status_from_progress(&request, &done);
        assert!(matches!(status, Status::Completed(Evidence::Picked { .. })));
        assert!(
            p.controller
                .borrow_mut()
                .update("op-p", "session-a", status)
        );
        p.apply_progress(&request, Some(done));
        p.render();
        let marks = controls.stage_marks().unwrap();
        assert_eq!(marks[5], StageMark::Loaded);
        assert!(marks[..5].iter().all(|m| *m == StageMark::Done));
        assert_eq!(check_visible(p), [false, false, false, false, false, true]);
        assert!(
            p.reason
                .text()
                .contains("New session new-7 loaded the handoff")
        );
        assert_eq!(controls.announcements().len(), 7);
    });
}

/// Completion without pickup_loaded evidence, a stage that goes back and a
/// report for another operation never become success or replace progress.
#[test]
fn gtk_pickup_success_needs_loaded_stage_and_new_session_id() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let s = selected("session-a");
        let mut c = Controller::default();
        c.select(Some(s.clone()));
        let prompt = c.prepare(Action::Pickup).unwrap();
        let request = c.confirm(prompt, &s, "op-p".into()).unwrap();
        for bad in [
            progress(&request, Stage::PickupLoaded, State::Completed, None),
            progress(&request, Stage::PickupSent, State::Completed, Some("new")),
            // The new session cannot be the old one.
            progress(&request, Stage::PickupLoaded, State::Completed, Some("s")),
            {
                let mut p = progress(&request, Stage::PickupLoaded, State::Completed, Some("n"));
                p.handoff = None;
                p
            },
            {
                let mut p = progress(&request, Stage::PickupLoaded, State::Completed, Some("n"));
                p.old_session.session_id = "someone-else".into();
                p
            },
        ] {
            let status = status_from_progress(&request, &bad);
            assert_eq!(status, Status::Unknown, "{bad:?}");
            assert!(!stage_marks(&bad, &status).contains(&StageMark::Loaded));
        }

        let controls = SessionControls::new(false);
        let p = &controls.panel;
        *p.controller.borrow_mut() = c;
        p.apply_progress(
            &request,
            Some(progress(&request, Stage::Clearing, State::Running, None)),
        );
        p.apply_progress(
            &request,
            Some(progress(
                &request,
                Stage::HandoffRunning,
                State::Running,
                None,
            )),
        );
        let mut other = progress(&request, Stage::PickupLoaded, State::Completed, Some("x"));
        other.operation_id = "another-op".into();
        p.apply_progress(&request, Some(other));
        assert_eq!(p.progress.borrow().as_ref().unwrap().stage, Stage::Clearing);
        assert!(
            p.controller
                .borrow_mut()
                .update("op-p", "session-a", Status::Running)
        );
        p.render();
        assert_eq!(controls.stage_marks().unwrap()[2], StageMark::Now);
    });
}

/// Acceptance 2, GUI half: a failure injected at each stage through the
/// adapter port shows that stage in crimson, the stages after it as not
/// reached, the resume route, and keeps the old session selected.
#[test]
fn gtk_injected_failure_at_each_stage_keeps_the_old_session() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        for kind in ["fail", "unknown"] {
            for (n, stage) in STAGES.into_iter().enumerate() {
                let key = format!("pickup-{kind}-{n}");
                let starts = PICKUP_STARTS.load(std::sync::atomic::Ordering::SeqCst);
                let controls = run_pickup("pi", &key);
                let p = &controls.panel;
                assert_eq!(
                    PICKUP_STARTS.load(std::sync::atomic::Ordering::SeqCst) - starts,
                    1,
                    "{key}: sent exactly once"
                );
                let marks = controls.stage_marks().expect("stage list shown");
                let stop = if kind == "fail" {
                    StageMark::Stopped
                } else {
                    StageMark::Unknown
                };
                for (j, mark) in marks.iter().enumerate() {
                    let want = match j.cmp(&n) {
                        std::cmp::Ordering::Less => StageMark::Done,
                        std::cmp::Ordering::Equal => stop,
                        std::cmp::Ordering::Greater => StageMark::NotReached,
                    };
                    assert_eq!(*mark, want, "{key} row {j}");
                }
                let row = &p.stages.rows[n].state;
                assert!(
                    row.has_css_class("blocked") && !row.has_css_class("busy"),
                    "{key}"
                );
                assert!(
                    check_visible(p).iter().all(|v| !v),
                    "{key}: no success mark"
                );
                let text = p.reason.text();
                assert!(
                    text.contains(&format!("stage {} of 6", n + 1)),
                    "{key}: {text}"
                );
                assert!(text.contains(stage_name(stage)), "{key}: {text}");
                assert!(
                    text.contains("The old session is kept")
                        && text.contains("open the old session file /session.jsonl in Pi"),
                    "{key}: {text}"
                );
                // The old session stays the selection; nothing switched over.
                let c = p.controller.borrow();
                assert_eq!(c.selection.as_ref().unwrap().identity, key);
                assert_eq!(c.operation.as_ref().unwrap().0.session.identity, key);
                drop(c);
                let last = controls.announcements().pop().unwrap();
                assert!(
                    last.contains(&format!("stage {} of 6", n + 1)),
                    "{key}: {last}"
                );
                if kind == "unknown" {
                    // Unknown stays open: query the same ID, offer nothing else.
                    assert!(p.query.is_visible(), "{key}");
                    assert!(text.contains("do not send it again"));
                    assert!(p.buttons.iter().all(|(_, b)| !b.is_visible()));
                } else {
                    assert!(!p.query.is_visible(), "{key}");
                    assert!(text.contains("injected failure"), "{key}: {text}");
                }
                assert!(!p.cancel.is_visible(), "{key}: pickup has no cancel");
            }
        }
    });
}

/// Claude Code's resume route is `claude --resume <old id>`.
#[test]
fn gtk_claude_pickup_failure_shows_claude_resume_route() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        for key in ["pickup-fail-0", "pickup-unknown-0"] {
            let controls = run_pickup("claude", key);
            let text = controls.panel.reason.text();
            assert!(
                text.contains("run claude --resume old-claude-id"),
                "{key}: {text}"
            );
            assert!(
                text.contains("stage 1 of 6 (Writing the handoff)"),
                "{key}: {text}"
            );
        }
    });
}

/// Through the port to pickup_loaded: the only check icon is on stage 6, and
/// the new session in the same pane still sees the result.
#[test]
fn gtk_pickup_reaches_loaded_and_follows_the_new_session() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let controls = run_pickup("pi", "pickup-ok");
        let p = &controls.panel;
        let marks = controls.stage_marks().unwrap();
        assert_eq!(marks[5], StageMark::Loaded);
        assert_eq!(check_visible(p), [false, false, false, false, false, true]);
        assert!(p.reason.text().contains("New session new-session-7 loaded"));
        let said = controls.announcements();
        assert!(said.len() >= 2 && said.last().unwrap().contains("new-session-7"));
        // The sampler now reports the new native session in the same pane.
        let mut next = sampled("pi", "new-session-7", true);
        next.overlay.as_mut().unwrap().agent_session_id = Some("new-session-7".into());
        controls.set_session(Some(&next));
        settle(p);
        assert_eq!(controls.stage_marks().unwrap()[5], StageMark::Loaded);
        // Another session does not inherit it.
        let mut other = sampled("pi", "unrelated", true);
        other.overlay.as_mut().unwrap().agent_session_id = Some("unrelated".into());
        controls.set_session(Some(&other));
        settle(p);
        assert!(controls.stage_marks().is_none());
    });
}

/// The pickup confirmation states target, sequence, cost, history and resume.
#[test]
fn pickup_confirmation_states_target_sequence_cost_and_resume() {
    let s = selected("session-a");
    let mut c = Controller::default();
    c.select(Some(s));
    let body = c.prepare(Action::Pickup).unwrap().body();
    for fragment in [
        "Identity: session-a",
        "Writes a handoff and waits for it to be saved",
        "new session in the same pane",
        "Loads the handoff",
        "may consume tokens",
        "history stays on disk",
        "open the old session file /session.jsonl in Pi",
    ] {
        assert!(body.contains(fragment), "{fragment}: {body}");
    }
}

// ---- Claude Code path (CC4): contract outcome -> UI status ----

fn claude_request(action: Action, id: &str) -> Request {
    let mut s = selected("claude-a");
    s.agent = Agent::Claude;
    s.cancel_supported = false;
    Request {
        operation_id: id.into(),
        session: s,
        action,
    }
}

fn outcome(
    id: &str,
    state: State,
    evidence: Option<agentcenter_core::control::Evidence>,
) -> agentcenter_core::control::Outcome {
    agentcenter_core::control::Outcome {
        operation_id: id.into(),
        action: agentcenter_core::control::Action::Handoff,
        state,
        code: "quota",
        evidence,
    }
}

/// Completion needs evidence for THIS request: another ID, no evidence, a
/// relative path, a short hash or a zero boundary is never "Saved".
#[test]
fn status_from_outcome_needs_matching_evidence() {
    use agentcenter_core::control::Evidence as E;
    let h = claude_request(Action::Handoff, "op-h");
    let artifact = |path: &str, sha: &str| {
        Some(E::Persisted(Artifact {
            path: path.into(),
            size: 7,
            sha256: sha.into(),
            modified_ms: 1_700_000_000_000,
        }))
    };
    let (ok, at) = status_from_outcome(
        &h,
        &outcome("op-h", State::Completed, artifact("/h/x.md", SHA)),
    );
    assert!(matches!(ok, Status::Completed(Evidence::Persisted { .. })));
    assert!(at.is_some(), "persisted time missing");
    for (o, why) in [
        (
            outcome("op-x", State::Completed, artifact("/h/x.md", SHA)),
            "other id",
        ),
        (outcome("op-h", State::Completed, None), "no evidence"),
        (
            outcome("op-h", State::Completed, artifact("h/x.md", SHA)),
            "relative",
        ),
        (
            outcome("op-h", State::Completed, artifact("/h/x.md", "ab")),
            "short sha",
        ),
        (
            outcome(
                "op-h",
                State::Completed,
                Some(E::Compacted { boundary_ms: 5 }),
            ),
            "compact evidence for a handoff",
        ),
    ] {
        assert_eq!(status_from_outcome(&h, &o).0, Status::Unknown, "{why}");
    }
    let c = claude_request(Action::Compact, "op-c");
    let done = |ms| {
        outcome(
            "op-c",
            State::Completed,
            Some(E::Compacted { boundary_ms: ms }),
        )
    };
    assert!(matches!(
        status_from_outcome(&c, &done(9)).0,
        Status::Completed(Evidence::Compacted { boundary_ms: 9 })
    ));
    assert_eq!(status_from_outcome(&c, &done(0)).0, Status::Unknown);
    let plain = |state| status_from_outcome(&c, &outcome("op-c", state, None)).0;
    assert_eq!(plain(State::Accepted), Status::Accepted);
    assert_eq!(plain(State::Running), Status::Sent);
    assert_eq!(plain(State::Rejected), Status::Refused("quota".into()));
    assert_eq!(plain(State::Failed), Status::Failed("quota".into()));
    assert_eq!(plain(State::Cancelled), Status::Cancelled);
    assert_eq!(plain(State::Unknown), Status::Unknown);
}

/// Sent (typed into the pane) moves on to Saved/Compacted only with evidence
/// and is rendered as such; Sent keeps Query, never the actions.
#[test]
fn gtk_claude_sent_moves_to_persisted_or_compacted() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        for (action, done) in [
            (
                Action::Handoff,
                Status::Completed(Evidence::Persisted {
                    path: "/h/x.md".into(),
                    size: 7,
                    sha256: SHA.into(),
                }),
            ),
            (
                Action::Compact,
                Status::Completed(Evidence::Compacted { boundary_ms: 9 }),
            ),
        ] {
            let p = SessionControls::new(false).panel;
            let s = claude_request(action, "op").session;
            {
                let mut c = p.controller.borrow_mut();
                c.select(Some(s.clone()));
                let prompt = c.prepare(action).unwrap();
                c.confirm(prompt, &s, "op".into()).unwrap();
                assert!(c.update("op", "claude-a", Status::Sent));
            }
            p.render();
            assert!(p.query.is_visible(), "{action:?}: Sent without Query");
            assert!(p.buttons.iter().all(|(_, b)| !b.is_visible()));
            assert!(!p.cancel.is_visible(), "Claude has no cancel");
            assert!(p.reason.text().contains("waiting for evidence"));
            // Out of order: an older Accepted never regresses Sent.
            assert!(
                !p.controller
                    .borrow_mut()
                    .update("op", "claude-a", Status::Accepted)
            );
            assert!(
                p.controller
                    .borrow_mut()
                    .update("op", "claude-a", done.clone())
            );
            p.render();
            assert_eq!(p.reason.text(), done.text(), "{action:?}");
            assert!(!p.query.is_visible());
        }
    });
}

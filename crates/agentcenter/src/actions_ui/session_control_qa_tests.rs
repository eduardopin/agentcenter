//! GTK presentation/scheduling regressions with injected controller state.
//! These tests do not exercise a socket, Pi, SDK, or persisted effects.
use super::*;

fn selected(identity: &str) -> Session {
    Session {
        identity: identity.into(),
        revision: "r1".into(),
        name: identity.into(),
        handoff_destination: "/private".into(),
        expected: serde_json::json!({"managerId":"m", "revision":"1", "sessionId":"s", "sessionFile":"/session.jsonl", "leafId":"old"}),
        handoff_disabled: None,
        compact_disabled: None,
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
        assert!(p.cancel.is_sensitive());
        assert!(!p.query.is_sensitive());
        assert!(p.buttons.iter().all(|(_, b)| !b.is_sensitive()));
        p.cancel.emit_clicked();
        assert!(p.cancel_pending.get());
        assert!(p.controller.borrow().cancel_requested);
        assert!(!p.cancel.is_sensitive());
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
        assert!(!p.reason.text().contains("Available"));
        assert!(
            p.reason
                .text()
                .contains("Unresolved operation for session-a")
        );
        assert!(p.buttons.iter().all(|(_, b)| !b.is_sensitive()));
        assert!(
            p.controller
                .borrow_mut()
                .update("op-a", "session-a", Status::Cancelled)
        );
        p.render();
        assert_eq!(p.controller.borrow().visible_status(), None);
        assert!(p.reason.text().contains("Available"));
        assert!(!p.reason.text().contains("Cancelled"));
        assert!(!p.cancel.is_sensitive());
        assert!(p.buttons.iter().all(|(_, b)| b.is_sensitive()));
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
        assert!(p.reason.text().contains("Handoff: model_auth_required"));
        assert!(p.reason.text().contains("Compact: busy"));
        assert!(!p.reason.text().contains("Available"));
        assert!(p.buttons.iter().all(|(_, b)| !b.is_sensitive()));
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
        assert!(
            p.reason
                .text()
                .contains("Unresolved operation for session-b")
        );
        assert!(p.query.label().unwrap().contains("session-b"));
        assert!(p.cancel.label().unwrap().contains("session-b"));
        assert!(p.buttons.iter().all(|(_, b)| !b.is_sensitive()));
    });
}

/// Owner rule (22/10): no dead controls. Without a controllable session the
/// panel shows only the reason; query/cancel appear only for an open operation.
#[test]
fn gtk_unsupported_selection_hides_every_control() {
    gtk::test_synced(|| {
        adw::init().unwrap();
        let controls = SessionControls::new(true);
        let p = &controls.panel;
        controls.set_session(None);
        assert!(p.buttons.iter().all(|(_, b)| !b.is_visible()));
        assert!(!p.refresh_button.is_visible());
        assert!(!p.query.is_visible());
        assert!(!p.cancel.is_visible());
        assert!(p.reason.is_visible());

        let running = running_panel();
        assert!(running.buttons.iter().all(|(_, b)| b.is_visible()));
        assert!(running.query.is_visible());
        assert!(running.cancel.is_visible());
        // Moving to an unsupported selection keeps the open operation reachable.
        running.controller.borrow_mut().select(None);
        running.render();
        assert!(running.buttons.iter().all(|(_, b)| !b.is_visible()));
        assert!(running.query.is_visible());
    });
}

static AUTHENTICATIONS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Counts discovery + authentication without touching a real runtime directory.
fn counting_authenticate(
    _: &agentcenter_core::Session,
) -> Result<(agentcenter_core::control::pi::Endpoint, Session), &'static str> {
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

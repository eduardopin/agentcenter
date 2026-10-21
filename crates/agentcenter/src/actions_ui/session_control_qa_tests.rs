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

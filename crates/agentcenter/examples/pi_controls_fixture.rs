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
fn main() {
    let args: Vec<String> = std::env::args().collect();
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
                0 if button(&root, "Handoff…").is_sensitive() => {
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
                    button(&root, "Refresh capabilities").emit_clicked();
                    stage.set(3);
                }
                3 if button(&root, "Compact…").is_sensitive() => {
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

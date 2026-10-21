//! AdwApplication: sobe o sampler, monta a janela e liga o feed ao laco do GTK.

use crate::feed;
use crate::measure::Probe;
use crate::prefs::Prefs;
use crate::window::Window;
use adw::prelude::*;
use agentcenter_core::{ProcRoot, Registry, Sampler, SamplerConfig};
use futures_util::StreamExt;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

/// Id da aplicacao (D-Bus, .desktop, metainfo, icone).
pub const APP_ID: &str = "io.github.eduardopin.AgentCenter";

/// Com `1`, nao procura instancia ja aberta (medicao, captura, testes manuais).
const NON_UNIQUE_ENV: &str = "AGENTCENTER_NON_UNIQUE";

/// Folhas da GUI num provedor so, tokens primeiro (as outras leem as
/// variaveis). Direcao em docs/DESIGN.md; contraste em scripts/contrast.py.
const STYLESHEET: &str = concat!(
    include_str!("../resources/tokens.css"),
    include_str!("../resources/summary.css"),
    include_str!("../resources/sessions.css"),
);

/// Carrega as folhas acima das do libadwaita. Desde o GTK 4.20 cada provedor
/// tem o proprio `prefers-color-scheme`, e o libadwaita so liga o dele: aqui o
/// nosso segue o AdwStyleManager (tema do sistema, ou ADW_DEBUG_COLOR_SCHEME
/// na captura), e tokens.css troca a camada semantica no `@media`.
fn load_css() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.connect_parsing_error(|_, section, error| {
        eprintln!("agentcenter: css {}: {error}", section.to_str());
    });
    let style = adw::StyleManager::default();
    follow_style(&provider, &style);
    let p = provider.clone();
    style.connect_dark_notify(move |s| follow_style(&p, s));
    let p = provider.clone();
    style.connect_high_contrast_notify(move |s| follow_style(&p, s));
    provider.load_from_string(STYLESHEET);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn follow_style(provider: &gtk::CssProvider, style: &adw::StyleManager) {
    provider.set_prefers_color_scheme(if style.is_dark() {
        gtk::InterfaceColorScheme::Dark
    } else {
        gtk::InterfaceColorScheme::Light
    });
    provider.set_prefers_contrast(if style.is_high_contrast() {
        gtk::InterfaceContrast::More
    } else {
        gtk::InterfaceContrast::NoPreference
    });
}

pub fn run(main_start: Instant) -> glib::ExitCode {
    // O sampler sobe antes do GTK: o primeiro tick (que le o smaps_rollup de
    // todos os processos de sessao) corre enquanto a janela e montada. No modo
    // fixture (scripts/capture-gui.sh) nao ha sampler: o feed repete o arquivo.
    // A selecao da lista vai ao sampler pelo `feed::Selection` (C11); no modo
    // fixture nao ha sampler e ela nao vai a lugar nenhum.
    // C19: o intervalo vem do GSettings (sem schema, 1 s), e o frame leva o
    // historico para as faixas do detalhe.
    let prefs = Prefs::load();
    let feed = match feed::Fixture::from_env() {
        Some(Ok(fixture)) => (feed::spawn(fixture, feed::Fixture::next), None),
        Some(Err(e)) => {
            eprintln!("agentcenter: {}: {e}", feed::SNAPSHOT_ENV);
            return glib::ExitCode::from(2);
        }
        None => {
            let config = SamplerConfig {
                interval: prefs.interval(),
                ..SamplerConfig::default()
            };
            let handle =
                Sampler::with_config(Registry::from_env(), config).spawn(ProcRoot::default(), true);
            let (live, selection) = feed::Live::new(handle);
            (feed::spawn(live, feed::Live::next), Some(selection))
        }
    };
    let feed = RefCell::new(Some(feed));

    let probe = Probe::from_env(main_start);
    let mut flags = gio::ApplicationFlags::empty();
    if probe.active() || std::env::var_os(NON_UNIQUE_ENV).is_some_and(|v| v == "1") {
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(flags)
        .build();

    let quit = gio::ActionEntry::builder("quit")
        .activate(|app: &adw::Application, _, _| app.quit())
        .build();
    app.add_action_entries([quit]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("window.close", &["<Control>w"]);

    app.connect_startup(|_| load_css());

    app.connect_activate(move |app| {
        if let Some(w) = app.active_window() {
            w.present();
            return;
        }
        let Some(((latest, mut wake), selection)) = feed.take() else {
            return;
        };
        let live = selection.is_some();
        if let Some(s) = &selection {
            let s = s.clone();
            prefs.connect_interval(move |d| s.request_interval(d));
        }
        let win = Rc::new(Window::new(app, live, &prefs, move |pid| {
            if let Some(s) = &selection {
                s.request(pid);
            }
        }));
        let probe = probe.clone();
        probe.attach(&win.window, app);
        let w = Rc::clone(&win);
        glib::spawn_future_local(async move {
            // Um aviso por slot cheio; o slot ja tem o frame mais novo.
            while wake.next().await.is_some() {
                if let Some(frame) = latest.take() {
                    w.apply(frame);
                    probe.data_applied();
                }
            }
        });
        win.window.present();
    });
    app.run()
}

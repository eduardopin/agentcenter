//! Menu bar (File, View, Help), the theme choice and the About dialog.
//!
//! The theme is an `app.theme` action with a string state, so the View menu
//! shows it as radio items and the choice survives restarts through
//! [`Prefs::set_theme`]. A capture run (`ADW_DEBUG_COLOR_SCHEME`) keeps the
//! scheme it forces: the saved choice is neither applied nor overwritten.

use crate::prefs::Prefs;
use adw::prelude::*;
use gtk::{gio, glib};

/// Theme choice in View > Theme, stored in GSettings as its [`Theme::id`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    pub fn id(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    pub fn from_id(id: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "Follow _System",
            Theme::Light => "_Light",
            Theme::Dark => "_Dark",
        }
    }

    pub fn color_scheme(self) -> adw::ColorScheme {
        match self {
            Theme::System => adw::ColorScheme::Default,
            Theme::Light => adw::ColorScheme::ForceLight,
            Theme::Dark => adw::ColorScheme::ForceDark,
        }
    }
}

/// `YYYY-MM-DD` (UTC) of a Unix time in seconds.
pub fn build_date(epoch: u64) -> String {
    // Days to civil date, Howard Hinnant's algorithm (proleptic Gregorian).
    let z = (epoch / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// "0.1.0 (61f1b7c)", or the bare version when the build had no git.
pub fn version_label(version: &str, rev: &str) -> String {
    if rev.is_empty() {
        version.to_string()
    } else {
        format!("{version} ({rev})")
    }
}

/// The menu bar model: File, View, Help.
pub fn model() -> gio::Menu {
    let file = gio::Menu::new();
    file.append(Some("_Quit"), Some("app.quit"));

    let themes = gio::Menu::new();
    for t in Theme::ALL {
        let item = gio::MenuItem::new(Some(t.label()), None);
        item.set_action_and_target_value(Some("app.theme"), Some(&t.id().to_variant()));
        themes.append_item(&item);
    }
    let view = gio::Menu::new();
    view.append_section(Some("Theme"), &themes);

    let help = gio::Menu::new();
    help.append(Some("_About AgentCenter"), Some("app.about"));

    let bar = gio::Menu::new();
    bar.append_submenu(Some("_File"), &file);
    bar.append_submenu(Some("_View"), &view);
    bar.append_submenu(Some("_Help"), &help);
    bar
}

/// The menu bar's three menus as (label with mnemonic, model), in bar order:
/// the header shows them as buttons, built from the same [`model`].
pub fn parts() -> Vec<(String, gio::MenuModel)> {
    let bar = model();
    (0..bar.n_items())
        .filter_map(|i| {
            let label = bar
                .item_attribute_value(i, "label", Some(glib::VariantTy::STRING))?
                .get::<String>()?;
            Some((label, bar.item_link(i, "submenu")?))
        })
        .collect()
}

/// Adds `app.theme` and `app.about` and applies the saved theme, unless
/// `capturing` (a capture forces its own scheme).
pub fn install(app: &adw::Application, prefs: &Prefs, capturing: bool) {
    let saved = if capturing {
        Theme::System
    } else {
        prefs.theme()
    };
    if !capturing {
        adw::StyleManager::default().set_color_scheme(saved.color_scheme());
    }
    let theme = gio::SimpleAction::new_stateful(
        "theme",
        Some(glib::VariantTy::STRING),
        &saved.id().to_variant(),
    );
    let p = prefs.clone();
    theme.connect_change_state(move |action, value| {
        let Some(t) = value
            .and_then(|v| v.get::<String>())
            .and_then(|id| Theme::from_id(&id))
        else {
            return;
        };
        adw::StyleManager::default().set_color_scheme(t.color_scheme());
        action.set_state(&t.id().to_variant());
        if !capturing {
            p.set_theme(t);
        }
    });
    app.add_action(&theme);

    let about = gio::SimpleAction::new("about", None);
    let a = app.downgrade();
    about.connect_activate(move |_, _| {
        let parent = a.upgrade().and_then(|app| app.active_window());
        about_dialog().present(parent.as_ref());
    });
    app.add_action(&about);
}

/// The About dialog: name, version, build date, author, licence, website.
pub fn about_dialog() -> adw::AboutDialog {
    let epoch = env!("AGENTCENTER_BUILD_EPOCH").parse().unwrap_or(0);
    let date = build_date(epoch);
    adw::AboutDialog::builder()
        .application_name("AgentCenter")
        .application_icon(crate::app::APP_ID)
        .version(version_label(
            env!("CARGO_PKG_VERSION"),
            env!("AGENTCENTER_GIT_REV"),
        ))
        .comments(format!(
            "Resource monitor for AI coding sessions on Linux.\nBuilt on {date}."
        ))
        .developer_name("Eduardo Pin")
        .developers(["Eduardo Pin"])
        .copyright("© 2026 Eduardo José Pin and the AgentCenter contributors")
        .license_type(gtk::License::MitX11)
        .website("https://github.com/eduardopin/agentcenter")
        .issue_url("https://github.com/eduardopin/agentcenter/issues")
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_ids_round_trip_and_map_to_adw_schemes() {
        assert_eq!(Theme::default(), Theme::System);
        for t in Theme::ALL {
            assert_eq!(Theme::from_id(t.id()), Some(t));
        }
        assert_eq!(
            Theme::ALL.map(Theme::id),
            ["system", "light", "dark"],
            "ids are the GSettings choices"
        );
        assert_eq!(Theme::from_id("solarized"), None);
        assert_eq!(Theme::System.color_scheme(), adw::ColorScheme::Default);
        assert_eq!(Theme::Light.color_scheme(), adw::ColorScheme::ForceLight);
        assert_eq!(Theme::Dark.color_scheme(), adw::ColorScheme::ForceDark);
        assert_eq!(
            Theme::ALL.map(Theme::label),
            ["Follow _System", "_Light", "_Dark"]
        );
    }

    #[test]
    fn build_date_is_the_utc_calendar_day() {
        assert_eq!(build_date(0), "1970-01-01");
        assert_eq!(build_date(86_399), "1970-01-01");
        assert_eq!(build_date(86_400), "1970-01-02");
        assert_eq!(build_date(951_782_400), "2000-02-29");
        assert_eq!(build_date(1_709_164_800), "2024-02-29");
        assert_eq!(build_date(1_792_800_000), "2026-10-24");
    }

    #[test]
    fn version_label_adds_the_commit_when_known() {
        assert_eq!(version_label("0.1.0", "61f1b7c"), "0.1.0 (61f1b7c)");
        assert_eq!(version_label("0.1.0", ""), "0.1.0");
    }

    /// (label, action, target) of every item, depth-first, with submenu labels.
    fn items(menu: &gio::MenuModel, out: &mut Vec<(String, String, String)>) {
        for i in 0..menu.n_items() {
            let label = menu
                .item_attribute_value(i, "label", Some(glib::VariantTy::STRING))
                .and_then(|v| v.get::<String>())
                .unwrap_or_default();
            let action = menu
                .item_attribute_value(i, "action", Some(glib::VariantTy::STRING))
                .and_then(|v| v.get::<String>())
                .unwrap_or_default();
            let target = menu
                .item_attribute_value(i, "target", None)
                .and_then(|v| v.get::<String>())
                .unwrap_or_default();
            out.push((label, action, target));
            for link in ["submenu", "section"] {
                if let Some(sub) = menu.item_link(i, link) {
                    items(&sub, out);
                }
            }
        }
    }

    #[test]
    fn menu_bar_has_file_view_help_with_quit_theme_and_about() {
        let m = model();
        let top: Vec<String> = (0..m.n_items())
            .map(|i| {
                m.item_attribute_value(i, "label", Some(glib::VariantTy::STRING))
                    .and_then(|v| v.get::<String>())
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(top, ["_File", "_View", "_Help"]);
        let mut all = Vec::new();
        items(m.upcast_ref(), &mut all);
        let has = |a: &str, t: &str| all.iter().any(|(_, x, y)| x == a && y == t);
        assert!(has("app.quit", ""));
        assert!(has("app.about", ""));
        for t in Theme::ALL {
            assert!(has("app.theme", t.id()), "View > Theme > {}", t.label());
        }
    }

    #[test]
    fn about_names_version_date_author_and_licence() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let d = about_dialog();
            assert_eq!(d.application_name(), "AgentCenter");
            assert_eq!(d.developer_name(), "Eduardo Pin");
            assert!(d.version().starts_with(env!("CARGO_PKG_VERSION")));
            assert_eq!(d.license_type(), gtk::License::MitX11);
            let date = build_date(env!("AGENTCENTER_BUILD_EPOCH").parse().unwrap());
            assert!(
                d.release_notes_version().contains(&date) || d.comments().contains(&date),
                "build date {date} shown"
            );
            assert!(d.website().contains("github.com/eduardopin/agentcenter"));
        });
    }

    #[test]
    fn theme_action_applies_and_saves_the_choice() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let dir = crate::prefs::test_support::compiled("menu-save");
            let prefs = crate::prefs::test_support::memory_prefs(&dir);
            let style = adw::StyleManager::default();
            style.set_color_scheme(adw::ColorScheme::Default);
            let app = adw::Application::new(None, gio::ApplicationFlags::NON_UNIQUE);
            install(&app, &prefs, false);
            // An unregistered GApplication does not route its action group:
            // drive the action from the map the menu items name.
            let theme = app.lookup_action("theme").expect("app.theme");
            theme.activate(Some(&"dark".to_variant()));
            assert_eq!(style.color_scheme(), adw::ColorScheme::ForceDark);
            assert_eq!(
                theme.state().and_then(|v| v.get::<String>()),
                Some("dark".into())
            );
            assert_eq!(prefs.theme(), Theme::Dark, "the choice is saved");
            theme.activate(Some(&"light".to_variant()));
            assert_eq!(style.color_scheme(), adw::ColorScheme::ForceLight);
            assert_eq!(prefs.theme(), Theme::Light);
            theme.activate(Some(&"system".to_variant()));
            assert_eq!(style.color_scheme(), adw::ColorScheme::Default);
            assert_eq!(prefs.theme(), Theme::System);
            assert!(app.lookup_action("about").is_some());
        });
    }

    #[test]
    fn startup_applies_the_saved_theme() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let dir = crate::prefs::test_support::compiled("menu-start");
            let prefs = crate::prefs::test_support::memory_prefs(&dir);
            prefs.set_theme(Theme::Dark);
            let style = adw::StyleManager::default();
            style.set_color_scheme(adw::ColorScheme::Default);
            let app = adw::Application::new(None, gio::ApplicationFlags::NON_UNIQUE);
            install(&app, &prefs, false);
            assert_eq!(style.color_scheme(), adw::ColorScheme::ForceDark);
            assert_eq!(
                app.lookup_action("theme")
                    .and_then(|a| a.state())
                    .and_then(|v| v.get::<String>()),
                Some("dark".into())
            );
        });
    }

    #[test]
    fn capture_mode_keeps_its_scheme() {
        gtk::test_synced(|| {
            adw::init().expect("isolated Broadway display required");
            let dir = crate::prefs::test_support::compiled("menu-capture");
            let prefs = crate::prefs::test_support::memory_prefs(&dir);
            prefs.set_theme(Theme::Dark);
            let style = adw::StyleManager::default();
            style.set_color_scheme(adw::ColorScheme::Default);
            let app = adw::Application::new(None, gio::ApplicationFlags::NON_UNIQUE);
            install(&app, &prefs, true);
            // A capture neither applies the saved choice at startup...
            assert_eq!(style.color_scheme(), adw::ColorScheme::Default);
            // ...nor overwrites it when the theme changes during the capture.
            let theme = app.lookup_action("theme").expect("app.theme");
            theme.activate(Some(&"light".to_variant()));
            assert_eq!(prefs.theme(), Theme::Dark);
            style.set_color_scheme(adw::ColorScheme::Default);
        });
    }
}

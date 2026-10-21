//! Preferencias em GSettings (C19): intervalo do tick, ultima ordem da lista e
//! tamanho da janela. Schema em `data/io.github.eduardopin.AgentCenter.gschema.xml`.
//!
//! Sem o schema instalado (rodando do `target/`, sem `GSETTINGS_SCHEMA_DIR`),
//! `gio::Settings::new` abortaria o processo. Aqui o schema e procurado antes:
//! ausente, a app roda nos padroes do schema e nao grava nada. Valor lido fora
//! da faixa (schema antigo, dconf editado a mao) cai para dentro dela; valor
//! gravado e preso na faixa antes, porque `g_settings_set` fora dela so avisa.

use crate::session_list::SortKey;
use gtk::gio;
use gtk::prelude::*;
use std::time::Duration;

/// Mesmo id da aplicacao (app.rs).
pub const SCHEMA_ID: &str = crate::app::APP_ID;

pub const KEY_INTERVAL: &str = "tick-interval-ms";
pub const KEY_SORT: &str = "sort-order";
pub const KEY_WIDTH: &str = "window-width";
pub const KEY_HEIGHT: &str = "window-height";
pub const KEY_MAXIMIZED: &str = "window-maximized";

/// Faixas e padroes do schema, repetidos aqui para o modo sem schema; o teste
/// `defaults_match_the_schema` confere que nao divergem.
pub const INTERVAL_MS: (u32, u32, u32) = (250, 10_000, 1000);
pub const WIDTH: (i32, i32, i32) = (360, 32_767, 1180);
pub const HEIGHT: (i32, i32, i32) = (294, 32_767, 720);

/// Tamanho da janela guardado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSize {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

impl Default for WindowSize {
    fn default() -> Self {
        Self {
            width: WIDTH.2,
            height: HEIGHT.2,
            maximized: false,
        }
    }
}

#[derive(Clone)]
pub struct Prefs {
    settings: Option<gio::Settings>,
}

impl Prefs {
    /// Pelo caminho padrao do GIO (`XDG_DATA_DIRS`, `GSETTINGS_SCHEMA_DIR`) e
    /// o backend padrao (dconf, ou `GSETTINGS_BACKEND`).
    pub fn load() -> Self {
        let p = Self::from_source(gio::SettingsSchemaSource::default().as_ref(), None);
        if !p.is_persistent() {
            eprintln!(
                "agentcenter: GSettings schema {SCHEMA_ID} is not installed; \
                 using the defaults, preferences are not saved"
            );
        }
        p
    }

    /// Com uma fonte e um backend dados (teste). `None` na fonte: sem schema.
    pub fn from_source(
        source: Option<&gio::SettingsSchemaSource>,
        backend: Option<&gio::SettingsBackend>,
    ) -> Self {
        let settings = source
            .and_then(|s| s.lookup(SCHEMA_ID, true))
            .filter(|schema| {
                [KEY_INTERVAL, KEY_SORT, KEY_WIDTH, KEY_HEIGHT, KEY_MAXIMIZED]
                    .iter()
                    .all(|k| schema.has_key(k))
            })
            .map(|schema| gio::Settings::new_full(&schema, backend, None));
        Self { settings }
    }

    /// Sem schema: padroes, e nada e gravado.
    pub fn is_persistent(&self) -> bool {
        self.settings.is_some()
    }

    pub fn interval(&self) -> Duration {
        let (min, max, def) = INTERVAL_MS;
        let ms = self.settings.as_ref().map_or(def, |s| s.uint(KEY_INTERVAL));
        Duration::from_millis(u64::from(ms.clamp(min, max)))
    }

    pub fn sort_key(&self) -> SortKey {
        self.settings
            .as_ref()
            .and_then(|s| SortKey::from_id(&s.string(KEY_SORT)))
            .unwrap_or_default()
    }

    pub fn set_sort_key(&self, key: SortKey) {
        if let Some(s) = &self.settings
            && s.string(KEY_SORT) != key.id()
        {
            let _ = s.set_string(KEY_SORT, key.id());
        }
    }

    pub fn window_size(&self) -> WindowSize {
        let Some(s) = &self.settings else {
            return WindowSize::default();
        };
        WindowSize {
            width: s.int(KEY_WIDTH).clamp(WIDTH.0, WIDTH.1),
            height: s.int(KEY_HEIGHT).clamp(HEIGHT.0, HEIGHT.1),
            maximized: s.boolean(KEY_MAXIMIZED),
        }
    }

    /// Grava o tamanho preso nas faixas do schema. Maximizada, so a marca: o
    /// tamanho guardado fica o de antes de maximizar.
    pub fn set_window_size(&self, size: WindowSize) {
        let Some(s) = &self.settings else { return };
        let _ = s.set_boolean(KEY_MAXIMIZED, size.maximized);
        if !size.maximized {
            let _ = s.set_int(KEY_WIDTH, size.width.clamp(WIDTH.0, WIDTH.1));
            let _ = s.set_int(KEY_HEIGHT, size.height.clamp(HEIGHT.0, HEIGHT.1));
        }
    }

    /// `f` com o intervalo novo quando a chave muda (gsettings set, dconf-editor).
    pub fn connect_interval(&self, f: impl Fn(Duration) + 'static) {
        let Some(s) = &self.settings else { return };
        let p = self.clone();
        s.connect_changed(Some(KEY_INTERVAL), move |_, _| f(p.interval()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    const SCHEMA_XML: &str =
        include_str!("../../../data/io.github.eduardopin.AgentCenter.gschema.xml");
    const CHILD_ENV: &str = "AGENTCENTER_PREFS_CHILD";

    /// Diretorio novo em target/tmp (nunca /tmp), apagado no fim do teste.
    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/tmp")
                .join(format!("prefs-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).expect("create target/tmp dir");
            Self(p)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// O schema de data/ compilado num diretorio de target/tmp.
    fn compiled(name: &str) -> Dir {
        let dir = Dir::new(name);
        let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data");
        let st = Command::new("glib-compile-schemas")
            .arg("--strict")
            .arg("--targetdir")
            .arg(&dir.0)
            .arg(&data)
            .status()
            .expect("glib-compile-schemas (libglib2.0-bin) is needed by this test");
        assert!(st.success(), "glib-compile-schemas failed: {st}");
        dir
    }

    fn source(dir: &Dir) -> gio::SettingsSchemaSource {
        gio::SettingsSchemaSource::from_directory(&dir.0, None, false).expect("schema source")
    }

    #[test]
    fn missing_schema_runs_on_the_defaults_and_saves_nothing() {
        let empty = Dir::new("empty");
        // Diretorio sem gschemas.compiled: o GIO recusa a fonte.
        let src = gio::SettingsSchemaSource::from_directory(&empty.0, None, false).ok();
        for p in [
            Prefs::from_source(None, None),
            Prefs::from_source(src.as_ref(), None),
        ] {
            assert!(!p.is_persistent());
            assert_eq!(p.interval(), Duration::from_secs(1));
            assert_eq!(p.sort_key(), SortKey::State);
            assert_eq!(p.window_size(), WindowSize::default());
            // Nada a gravar e nada quebra.
            p.set_sort_key(SortKey::Cpu);
            p.set_window_size(WindowSize {
                width: 1,
                height: 1,
                maximized: true,
            });
            p.connect_interval(|_| panic!("no settings, no signal"));
            assert_eq!(p.sort_key(), SortKey::State);
        }
    }

    #[test]
    fn schema_round_trips_through_a_memory_backend() {
        let dir = compiled("round");
        let backend = gio::memory_settings_backend_new();
        let p = Prefs::from_source(Some(&source(&dir)), Some(&backend));
        assert!(p.is_persistent());
        assert_eq!(p.interval(), Duration::from_secs(1));
        assert_eq!(p.window_size(), WindowSize::default());

        p.set_sort_key(SortKey::Recommendation);
        assert_eq!(p.sort_key(), SortKey::Recommendation);
        // Abaixo da faixa: preso no minimo, sem aviso do GIO.
        p.set_window_size(WindowSize {
            width: 200,
            height: 900,
            maximized: false,
        });
        assert_eq!(
            p.window_size(),
            WindowSize {
                width: 360,
                height: 900,
                maximized: false
            }
        );
        // Maximizada: o tamanho de antes fica.
        p.set_window_size(WindowSize {
            width: 3000,
            height: 2000,
            maximized: true,
        });
        let w = p.window_size();
        assert!(w.maximized);
        assert_eq!((w.width, w.height), (360, 900));
    }

    #[test]
    fn defaults_match_the_schema() {
        let def = |key: &str| {
            let at = SCHEMA_XML.find(&format!("name=\"{key}\"")).unwrap();
            let rest = &SCHEMA_XML[at..];
            let a = rest.find("<default>").unwrap() + "<default>".len();
            rest[a..a + rest[a..].find("</default>").unwrap()].to_string()
        };
        assert_eq!(def(KEY_INTERVAL), INTERVAL_MS.2.to_string());
        assert_eq!(def(KEY_WIDTH), WIDTH.2.to_string());
        assert_eq!(def(KEY_HEIGHT), HEIGHT.2.to_string());
        assert_eq!(def(KEY_SORT), format!("'{}'", SortKey::default().id()));
        assert!(SCHEMA_XML.contains(&format!(
            "<range min=\"{}\" max=\"{}\"/>",
            INTERVAL_MS.0, INTERVAL_MS.1
        )));
        assert!(SCHEMA_XML.contains(&format!("<range min=\"{}\" max=\"{}\"/>", WIDTH.0, WIDTH.1)));
    }

    /// Corre so como filho de [`load_follows_gsettings_schema_dir`]: o caminho
    /// padrao do GIO le `GSETTINGS_SCHEMA_DIR` uma vez por processo, entao
    /// cada caso precisa de processo proprio (e mexer no ambiente com outros
    /// testes rodando seria corrida).
    #[test]
    #[ignore = "child process of load_follows_gsettings_schema_dir"]
    fn load_child() {
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        let p = Prefs::load();
        println!("persistent={}", p.is_persistent());
        if p.is_persistent() {
            p.set_sort_key(SortKey::Name);
            assert_eq!(p.sort_key(), SortKey::Name);
        }
        assert_eq!(p.interval(), Duration::from_secs(1));
    }

    fn run_child(schema_dir: &Dir) -> String {
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "prefs::tests::load_child",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .env("GSETTINGS_SCHEMA_DIR", &schema_dir.0)
            .env("GSETTINGS_BACKEND", "memory")
            .output()
            .expect("run the test binary");
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "child failed: {text}");
        text
    }

    #[test]
    fn load_follows_gsettings_schema_dir() {
        let dir = compiled("env");
        assert!(run_child(&dir).contains("persistent=true"));
        // Sem o schema em lugar nenhum: padroes, sem panic. Se o schema estiver
        // instalado no sistema, o filho o acha mesmo assim; o que vale e nao cair.
        let installed = gio::SettingsSchemaSource::default()
            .and_then(|s| s.lookup(SCHEMA_ID, true))
            .is_some();
        let empty = Dir::new("env-empty");
        let text = run_child(&empty);
        if !installed {
            assert!(text.contains("persistent=false"), "{text}");
            assert!(text.contains("is not installed"), "{text}");
        }
    }
}

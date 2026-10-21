//! Modos de medicao, ligados por variavel de ambiente (scripts/measure-gui.sh):
//!
//! - `AGENTCENTER_EXIT_AFTER_FIRST_FRAME=1`: imprime
//!   `first_frame_ms=<a> first_data_ms=<b> origin=<exec|main>` e sai. `a` e o
//!   primeiro frame pintado da janela; `b`, o primeiro pintado com dados do
//!   sampler. A origem e `AGENTCENTER_T0_NS` (ns desde a epoca Unix, gravado
//!   pelo script logo antes do exec) quando existe, senao o inicio do `main`
//!   (que nao conta a carga das bibliotecas).
//! - `AGENTCENTER_SCREENSHOT=<arquivo.png>`: espera `SCREENSHOT_FRAMES` frames
//!   de dados (do segundo em diante ja ha taxas; 12 com `AGENTCENTER_PAGE=reclaim`,
//!   para a janela do reclaim encher), salva a janela em PNG e sai.
//!   `AGENTCENTER_SCREENSHOT_SIZE=<L>x<A>` troca o tamanho da janela. Antes
//!   de salvar, confere a largura ([`overflow`]): cada rolagem lateral com
//!   conteudo alem da pagina (`hadjustment.upper > page_size`), widget fora
//!   da janela e rotulo menor que o proprio minimo sai no stderr como
//!   `agentcenter: overflow: ...`, e o capture-gui.sh falha. Tambem imprime
//!   `band_px=<altura>` da faixa da maquina.

use gtk::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Frames de dados antes da captura: o primeiro tick nao tem taxas.
const SCREENSHOT_FRAMES: u32 = 3;
/// Na pagina Reclaim (`AGENTCENTER_PAGE=reclaim`): a janela do reclaim tem
/// [`crate::reclaim_page::WINDOW_TICKS`] ticks com taxas, e o primeiro do
/// sampler nao tem; 12 frames poem a captura com a lista ja montada, nao no
/// "watching".
const RECLAIM_SCREENSHOT_FRAMES: u32 = 12;

/// Frames antes da captura para a pagina pedida em [`crate::window::PAGE_ENV`].
fn screenshot_frames(page: Option<&str>) -> u32 {
    if page.is_some_and(|p| p.trim() == crate::window::VIEW_RECLAIM) {
        RECLAIM_SCREENSHOT_FRAMES
    } else {
        SCREENSHOT_FRAMES
    }
}

enum Origin {
    Exec(u128),
    Main(Instant),
}

impl Origin {
    fn elapsed_ms(&self) -> f64 {
        match self {
            Origin::Exec(t0) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos());
                now.saturating_sub(*t0) as f64 / 1e6
            }
            Origin::Main(t) => t.elapsed().as_secs_f64() * 1e3,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Origin::Exec(_) => "exec",
            Origin::Main(_) => "main",
        }
    }
}

struct State {
    origin: Origin,
    exit_after_first_frame: bool,
    screenshot: Option<String>,
    size: Option<(i32, i32)>,
    /// Frames de dados antes da captura ([`screenshot_frames`]).
    frames: u32,
    first_frame_ms: Cell<Option<f64>>,
    data_frames: Cell<u32>,
    done: Cell<bool>,
    /// Capturas que sairam vazias e foram repetidas ([`EMPTY_RETRIES`]).
    empty_tries: Cell<u32>,
    /// Para pedir a pintura que dispara a captura (ver `data_applied`).
    window: RefCell<Option<glib::WeakRef<gtk::Window>>>,
}

/// Quantas vezes a captura vazia e repetida na pintura seguinte. O
/// `WidgetPaintable` sai vazio quando a janela tem alocacao pendente; desde o
/// historico do C19 as leituras mudam a cada tick e um tick entre a pintura e
/// a captura pedia nova alocacao (metade das capturas do detalhe falhava).
const EMPTY_RETRIES: u32 = 10;

/// Sonda compartilhada entre a janela e o laco do feed. Inativa: custo zero.
#[derive(Clone)]
pub struct Probe(Option<Rc<State>>);

fn parse_size(v: &str) -> Option<(i32, i32)> {
    let (w, h) = v.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

impl Probe {
    pub fn from_env(main_start: Instant) -> Self {
        let exit_after_first_frame =
            std::env::var_os("AGENTCENTER_EXIT_AFTER_FIRST_FRAME").is_some_and(|v| v == "1");
        let screenshot = std::env::var("AGENTCENTER_SCREENSHOT")
            .ok()
            .filter(|p| !p.is_empty());
        if !exit_after_first_frame && screenshot.is_none() {
            return Self(None);
        }
        let origin = std::env::var("AGENTCENTER_T0_NS")
            .ok()
            .and_then(|v| v.trim().parse::<u128>().ok())
            .map_or(Origin::Main(main_start), Origin::Exec);
        let size = std::env::var("AGENTCENTER_SCREENSHOT_SIZE")
            .ok()
            .and_then(|v| parse_size(&v));
        Self(Some(Rc::new(State {
            origin,
            exit_after_first_frame,
            screenshot,
            size,
            frames: screenshot_frames(std::env::var(crate::window::PAGE_ENV).ok().as_deref()),
            first_frame_ms: Cell::new(None),
            data_frames: Cell::new(0),
            empty_tries: Cell::new(0),
            done: Cell::new(false),
            window: RefCell::new(None),
        })))
    }

    pub fn active(&self) -> bool {
        self.0.is_some()
    }

    /// Chamado pelo feed depois de cada frame aplicado.
    pub fn data_applied(&self) {
        let Some(s) = &self.0 else {
            return;
        };
        let data = s.data_frames.get() + 1;
        s.data_frames.set(data);
        // A captura sai no after-paint. Um frame igual ao anterior (o modo
        // fixture repete o mesmo snapshot) nao muda widget nenhum, entao nada
        // repinta e a captura esperava uma pintura alheia: 4 a 20 s, e acima
        // dos 30 s do capture-gui.sh o PNG nao saia. Pede a pintura aqui.
        if s.screenshot.is_some() && data >= s.frames && !s.done.get() {
            let window = s.window.borrow().as_ref().and_then(glib::WeakRef::upgrade);
            if let Some(w) = window {
                w.queue_draw();
            }
        }
    }

    pub fn attach(&self, window: &adw::ApplicationWindow, app: &adw::Application) {
        let Some(state) = self.0.clone() else {
            return;
        };
        if let Some((w, h)) = state.size {
            window.set_default_size(w, h);
        }
        state
            .window
            .replace(Some(window.upcast_ref::<gtk::Window>().downgrade()));
        let app = app.clone();
        window.connect_realize(move |window| {
            let Some(clock) = window.frame_clock() else {
                return;
            };
            let (state, app, window) = (Rc::clone(&state), app.clone(), window.clone());
            clock.connect_after_paint(move |_| on_paint(&state, &app, window.upcast_ref()));
        });
    }
}

fn on_paint(s: &Rc<State>, app: &adw::Application, window: &gtk::Window) {
    if s.done.get() {
        return;
    }
    if s.first_frame_ms.get().is_none() {
        s.first_frame_ms.set(Some(s.origin.elapsed_ms()));
    }
    let data = s.data_frames.get();
    if s.exit_after_first_frame && data >= 1 {
        s.done.set(true);
        println!(
            "first_frame_ms={:.1} first_data_ms={:.1} origin={}",
            s.first_frame_ms.get().unwrap_or(0.0),
            s.origin.elapsed_ms(),
            s.origin.name()
        );
        app.quit();
        return;
    }
    if let Some(path) = &s.screenshot
        && data >= s.frames
    {
        s.done.set(true);
        let (path, app, window, s) = (path.clone(), app.clone(), window.clone(), Rc::clone(s));
        let asked = s.size.map(|(w, _)| w);
        // Fora do after-paint: tirar um snapshot da janela dentro da pintura
        // dela reentraria no frame clock. Prioridade alta: roda antes do
        // proximo tick do feed, que pediria nova alocacao.
        glib::idle_add_local_full(glib::Priority::HIGH, move || {
            let problems = overflow(&window, asked);
            match save_png(&window, &path) {
                Err(e) if e == EMPTY && s.empty_tries.get() < EMPTY_RETRIES => {
                    // Alocacao pendente: tenta de novo na pintura seguinte.
                    s.empty_tries.set(s.empty_tries.get() + 1);
                    s.done.set(false);
                    window.queue_draw();
                    return glib::ControlFlow::Break;
                }
                Ok(()) => {
                    for problem in problems {
                        eprintln!("agentcenter: overflow: {problem}");
                    }
                    if let Some(band) = find_by_class(window.upcast_ref(), "machine-header") {
                        println!("band_px={}", band.height());
                    }
                    println!("screenshot={path} empty_retries={}", s.empty_tries.get());
                }
                Err(e) => eprintln!("agentcenter: screenshot failed: {e}"),
            }
            app.quit();
            glib::ControlFlow::Break
        });
    }
}

/// O AdwHeaderBar passa 1 px de cada lado de proposito (a borda encosta na
/// da janela); passar disso e transbordo.
const OVERHANG_PX: f32 = 1.5;

/// Nome curto de um widget para a mensagem: tipo e classes CSS.
fn describe(w: &gtk::Widget) -> String {
    let classes = w.css_classes();
    if classes.is_empty() {
        w.type_().name().to_owned()
    } else {
        format!("{}.{}", w.type_().name(), classes.join("."))
    }
}

/// O que passa da largura da janela: a janela maior que a pedida (o minimo
/// do conteudo empurrou), rolagem lateral com conteudo alem da pagina, widget
/// visivel fora da janela, rotulo sem reticencias menor que o proprio minimo
/// (texto cortado). Vazio quando cabe.
pub fn overflow(window: &gtk::Window, asked_width: Option<i32>) -> Vec<String> {
    let mut out = Vec::new();
    let width = window.width();
    if let Some(asked) = asked_width
        && width > asked
    {
        out.push(format!(
            "window is {width} px wide, asked {asked}: the content minimum does not fit"
        ));
    }
    fn walk(w: &gtk::Widget, root: &gtk::Widget, width: f32, out: &mut Vec<String>) {
        // A barra de rolagem por cima (overlay) passa alguns px da borda de
        // proposito: e cromo, nao conteudo.
        if !w.is_mapped() || w.is::<gtk::Scrollbar>() {
            return;
        }
        if let Some(sw) = w.downcast_ref::<gtk::ScrolledWindow>() {
            let a = sw.hadjustment();
            if a.upper() - a.page_size() > 0.5 {
                out.push(format!(
                    "{}: hadjustment upper {} > page_size {}",
                    describe(w),
                    a.upper(),
                    a.page_size()
                ));
            }
        }
        if let Some(b) = w.compute_bounds(root)
            && (b.x() < -OVERHANG_PX || b.x() + b.width() > width + OVERHANG_PX)
        {
            out.push(format!(
                "{} spans x {:.0}..{:.0} of a {width:.0} px window",
                describe(w),
                b.x(),
                b.x() + b.width()
            ));
            // Os filhos de quem ja passou passam junto: um aviso basta.
            return;
        }
        // Texto cortado: o layout do rotulo (ja quebrado na largura dada)
        // mais largo que a caixa de conteudo. Reticencias sao corte de
        // proposito e ficam de fora.
        if let Some(l) = w.downcast_ref::<gtk::Label>()
            && l.ellipsize() == gtk::pango::EllipsizeMode::None
        {
            let (text, _) = l.layout().pixel_size();
            if text > l.width() {
                out.push(format!(
                    "{} {:?} cut: {} px for {text} px of text",
                    describe(w),
                    l.label(),
                    l.width()
                ));
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            walk(&c, root, width, out);
            child = c.next_sibling();
        }
    }
    let root = window.upcast_ref::<gtk::Widget>();
    let mut child = root.first_child();
    while let Some(c) = child {
        walk(&c, root, width as f32, &mut out);
        child = c.next_sibling();
    }
    out
}

fn find_by_class(w: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
    if w.has_css_class(class) {
        return Some(w.clone());
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(found) = find_by_class(&c, class) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

const EMPTY: &str = "empty window";

fn save_png(window: &gtk::Window, path: &str) -> Result<(), String> {
    let (w, h) = (window.width(), window.height());
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(w), f64::from(h));
    let node = snapshot.to_node().ok_or(EMPTY)?;
    let renderer = window.renderer().ok_or("window has no renderer")?;
    let texture: gdk::Texture = renderer.render_texture(&node, None);
    texture.save_to_png(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reclaim_capture_waits_for_a_full_window() {
        assert_eq!(screenshot_frames(None), SCREENSHOT_FRAMES);
        assert_eq!(screenshot_frames(Some("sessions")), SCREENSHOT_FRAMES);
        assert_eq!(
            screenshot_frames(Some(" reclaim ")),
            RECLAIM_SCREENSHOT_FRAMES
        );
        // O primeiro tick nao tem taxas e nao entra na janela.
        assert!(RECLAIM_SCREENSHOT_FRAMES as usize > crate::reclaim_page::WINDOW_TICKS + 1);
    }
}

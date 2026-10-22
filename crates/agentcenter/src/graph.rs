//! Faixa de historico do detalhe (C19): a regua da maquina (gauge.rs) estendida
//! no tempo. Cada faixa e um trilho (`strip`, a cor de `--ac-track`) com ate
//! duas series do ring buffer da sessao (`agentcenter_core::history`, uma
//! amostra por tick). A borda direita e agora; a esquerda, [`HISTORY_LEN`]
//! ticks atras. Valor ausente (`NaN`) quebra a linha: nunca vira 0.
//!
//! - Serie principal (`line.primary`): linha cheia com area embaixo, ambar
//!   (`--ac-mark-agents`): o consumo da propria sessao.
//! - Segunda serie (`line.secondary`): tracejada, ardosia
//!   (`--ac-mark-machine`). O traco e o canal, nao so a cor.
//! - Limite (`mark`): filete horizontal carmim (`--ac-mark-alert`), como a
//!   marca de 10 % da regua de pressao. Uma faixa pode ter mais de um (CC9:
//!   contexto com 70 % e 90 %).
//! - Trecho marcado (`mark`, CC9): barra carmim de [`BAND_PX`] no rodape, sob
//!   as amostras de um trecho (o sinal de loop nas tool calls/min). Forma e
//!   posicao distinguem do limite (filete de 1 px na altura do valor), entao
//!   a cor nao e o unico canal.
//!
//! O desenho e GtkSnapshot do proprio widget (caminho GSK, nada de cairo) com a
//! cor CSS do no (`color` em sessions.css, que le so tokens). Cada serie e um
//! filho com no CSS proprio, entao o tema troca as cores sem codigo. So
//! repinta quando os valores ou a escala mudam ([`Line::set`]): com o ring
//! cheio e o valor parado, um tick nao desenha nada.
//!
//! A parte sem GTK ([`nice_max`], [`points`], [`peak`], [`runs_at_least`],
//! [`band_spans`], [`mark_y`]) e testavel sem display.

use agentcenter_core::HISTORY_LEN;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::{Cell, RefCell};
use std::ops::Range;

/// Opacidade da area sob a serie principal, sobre a cor do token: a linha
/// carrega o dado, a area so da peso a forma.
pub const AREA_ALPHA: f32 = 0.22;
/// Espessura da linha em px.
pub const LINE_PX: f32 = 1.5;
/// Traco e vao da segunda serie, em px.
pub const DASH_PX: [f32; 2] = [4.0, 3.0];
/// Altura da barra de trecho marcado, em px: mais grossa que o filete de
/// limite (1 px) e sempre no rodape.
pub const BAND_PX: i32 = 3;
/// Largura minima da barra: uma amostra so ainda se ve.
pub const BAND_MIN_PX: f32 = 2.0;

/// Teto "redondo" da escala: o menor 1, 2 ou 5 x 10^k (ou, com `binary`, a
/// menor potencia de 2) que cobre `max`, nunca abaixo de `floor`. Sem leitura
/// (todo `NaN`), o proprio `floor`.
pub fn nice_max(max: f64, floor: f64, binary: bool) -> f64 {
    let floor = floor.max(f64::MIN_POSITIVE);
    if !max.is_finite() || max <= floor {
        return floor;
    }
    if binary {
        return 2f64.powi(max.log2().ceil() as i32);
    }
    let exp = max.log10().floor();
    let base = 10f64.powf(exp);
    let m = max / base;
    let step = [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .find(|s| m <= *s + 1e-9)
        .unwrap_or(10.0);
    step * base
}

/// Maior leitura das series (ignora `NaN`); `None` sem nenhuma.
pub fn peak<'a>(series: impl IntoIterator<Item = &'a [f32]>) -> Option<f64> {
    series
        .into_iter()
        .flatten()
        .filter(|v| v.is_finite())
        .map(|v| f64::from(*v))
        .reduce(f64::max)
}

/// Pontos de uma serie em ordem cronologica (`Ring::to_vec`) num retangulo de
/// `w` x `h` px, com `cap` amostras de largura: a mais nova na borda direita,
/// cada amostra a `w / (cap - 1)` da anterior. Com o ring incompleto a linha
/// comeca no meio, onde a sessao comecou. `max` e o topo; acima dele a linha
/// fica no topo. Um `NaN` fecha o trecho: devolve trechos, nao uma linha so.
pub fn points(values: &[f32], cap: usize, max: f64, w: f32, h: f32) -> Vec<Vec<(f32, f32)>> {
    let cap = cap.max(values.len()).max(2);
    let step = w / (cap - 1) as f32;
    let first = cap - values.len();
    let max = if max.is_finite() && max > 0.0 {
        max
    } else {
        1.0
    };
    let mut runs: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut run = Vec::new();
    for (i, v) in values.iter().enumerate() {
        if !v.is_finite() {
            if !run.is_empty() {
                runs.push(std::mem::take(&mut run));
            }
            continue;
        }
        let frac = (f64::from(*v) / max).clamp(0.0, 1.0) as f32;
        run.push(((first + i) as f32 * step, h - frac * h));
    }
    if !run.is_empty() {
        runs.push(run);
    }
    runs
}

/// Trechos de indices consecutivos em que `values` fica `>= at`. `NaN` (sem
/// leitura) e valor abaixo fecham o trecho: lacuna nunca vira trecho.
pub fn runs_at_least(values: &[f32], at: f32) -> Vec<Range<usize>> {
    let mut runs = Vec::new();
    let mut start = None;
    for (i, v) in values.iter().enumerate() {
        match (v.is_finite() && *v >= at, start) {
            (true, None) => start = Some(i),
            (false, Some(a)) => {
                runs.push(a..i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(a) = start {
        runs.push(a..values.len());
    }
    runs
}

/// Faixas horizontais `(x0, x1)` em px de trechos de amostras, na mesma
/// posicao de [`points`] (`len` amostras encostadas a direita de `cap`): meia
/// amostra para cada lado, no minimo [`BAND_MIN_PX`], sempre dentro de `[0, w]`.
pub fn band_spans(runs: &[Range<usize>], len: usize, cap: usize, w: f32) -> Vec<(f32, f32)> {
    let cap = cap.max(len).max(2);
    let step = w / (cap - 1) as f32;
    let first = cap - len;
    runs.iter()
        .filter(|r| r.start < r.end && r.end <= len)
        .map(|r| {
            let x0 = (first + r.start) as f32 * step - step / 2.0;
            let x1 = (first + r.end - 1) as f32 * step + step / 2.0;
            let (mut x0, mut x1) = (x0.max(0.0), x1.min(w));
            if x1 - x0 < BAND_MIN_PX {
                let mid = (x0 + x1) / 2.0;
                x0 = (mid - BAND_MIN_PX / 2.0).clamp(0.0, (w - BAND_MIN_PX).max(0.0));
                x1 = (x0 + BAND_MIN_PX).min(w);
            }
            (x0, x1)
        })
        .collect()
}

/// Linha (px, de cima) do filete de um limite em `at` (fracao da escala)
/// numa faixa de `height` px, com a mesma folga de [`LINE_PX`] da serie.
pub fn mark_y(at: f64, height: i32) -> i32 {
    let usable = height as f32 - LINE_PX;
    let y = (usable - at.clamp(0.0, 1.0) as f32 * usable).round() as i32;
    y.clamp(0, (height - 1).max(0))
}

/// Iguais bit a bit: `NaN == NaN` aqui (amostra ausente igual a ausente).
fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

// ---- Serie ------------------------------------------------------------------

mod line {
    use super::*;

    #[derive(Default)]
    pub struct Line {
        pub values: RefCell<Vec<f32>>,
        pub max: Cell<f64>,
        pub cap: Cell<usize>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Line {
        const NAME: &'static str = "AcStripLine";
        type Type = super::Line;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("line");
        }
    }

    impl ObjectImpl for Line {
        fn constructed(&self) {
            self.parent_constructed();
            self.cap.set(HISTORY_LEN);
            self.max.set(1.0);
            let obj = self.obj();
            obj.set_can_target(false);
            obj.set_focusable(false);
            obj.set_accessible_role(gtk::AccessibleRole::Presentation);
        }
    }

    impl WidgetImpl for Line {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let values = self.values.borrow();
            // Meia linha de folga em cima e embaixo: o traco no topo e no
            // zero nao e cortado.
            let inset = LINE_PX / 2.0;
            let runs = points(&values, self.cap.get(), self.max.get(), w, h - LINE_PX);
            let color = obj.color();
            let dashed = obj.has_css_class("secondary");
            let stroke = gtk::gsk::Stroke::new(LINE_PX);
            stroke.set_line_join(gtk::gsk::LineJoin::Round);
            if dashed {
                stroke.set_dash(&DASH_PX);
            }
            for run in &runs {
                if let &[(x, y)] = run.as_slice() {
                    // Amostra isolada entre dois ausentes: um ponto.
                    snapshot.append_color(
                        &color,
                        &gtk::graphene::Rect::new(x - LINE_PX, y, LINE_PX * 2.0, LINE_PX * 2.0),
                    );
                    continue;
                }
                let line = gtk::gsk::PathBuilder::new();
                line.move_to(run[0].0, run[0].1 + inset);
                for (x, y) in &run[1..] {
                    line.line_to(*x, y + inset);
                }
                if !dashed {
                    let area = gtk::gsk::PathBuilder::new();
                    let (x0, xn) = (run[0].0, run[run.len() - 1].0);
                    area.move_to(x0, h);
                    for (x, y) in run {
                        area.line_to(*x, y + inset);
                    }
                    area.line_to(xn, h);
                    area.close();
                    let mut fill = color;
                    fill.set_alpha(color.alpha() * AREA_ALPHA);
                    snapshot.append_fill(&area.to_path(), gtk::gsk::FillRule::Winding, &fill);
                }
                snapshot.append_stroke(&line.to_path(), &stroke, &color);
            }
        }
    }
}

glib::wrapper! {
    /// Uma serie da faixa. CSS: `strip > line.primary`, `strip > line.secondary`.
    pub struct Line(ObjectSubclass<line::Line>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Line {
    fn new(class: &str) -> Self {
        let l: Self = glib::Object::new();
        l.add_css_class(class);
        l
    }

    /// Troca valores, escala e capacidade; so pede pintura se algo mudou.
    /// Devolve se mudou.
    pub fn set(&self, values: &[f32], max: f64, cap: usize) -> bool {
        let imp = self.imp();
        if same(&imp.values.borrow(), values) && imp.max.get() == max && imp.cap.get() == cap {
            return false;
        }
        imp.values.replace(values.to_vec());
        imp.max.set(max);
        imp.cap.set(cap);
        self.queue_draw();
        true
    }
}

// ---- Faixa ------------------------------------------------------------------

mod strip {
    use super::*;

    #[derive(Default)]
    pub struct Strip {
        pub lines: RefCell<Vec<super::Line>>,
        /// Fracao da escala de cada limite; vazio sem limite.
        pub limits: RefCell<Vec<f64>>,
        /// Um filete por limite; o primeiro nasce com a faixa, os outros so
        /// quando uma faixa pede mais limites (nunca por tick).
        pub marks: RefCell<Vec<gtk::Widget>>,
        /// Trechos marcados (indices de amostra) e o comprimento e a
        /// capacidade da serie a que se referem.
        pub bands: RefCell<(Vec<Range<usize>>, usize, usize)>,
        /// Uma barra por trecho, reaproveitadas; crescem so com mais trechos.
        pub band_boxes: RefCell<Vec<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Strip {
        const NAME: &'static str = "AcStrip";
        type Type = super::Strip;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("strip");
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for Strip {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_focusable(false);
            obj.set_hexpand(true);
            let m = super::mark_box();
            m.set_parent(&*obj);
            self.marks.replace(vec![m]);
        }

        fn dispose(&self) {
            for l in self.lines.take() {
                l.unparent();
            }
            for m in self.marks.take() {
                m.unparent();
            }
            for b in self.band_boxes.take() {
                b.unparent();
            }
        }
    }

    impl WidgetImpl for Strip {
        fn measure(&self, _o: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            // Largura e altura minimas do CSS (min-height em sessions.css); a
            // faixa estica na largura que a grade der.
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let all = gtk::Allocation::new(0, 0, width, height);
            for l in self.lines.borrow().iter() {
                l.size_allocate(&all, -1);
            }
            for (m, at) in self.marks.borrow().iter().zip(self.limits.borrow().iter()) {
                m.size_allocate(
                    &gtk::Allocation::new(0, super::mark_y(*at, height), width, 1),
                    -1,
                );
            }
            let (runs, len, cap) = &*self.bands.borrow();
            let spans = super::band_spans(runs, *len, *cap, width as f32);
            let y = (height - BAND_PX).max(0);
            for (b, (x0, x1)) in self.band_boxes.borrow().iter().zip(spans) {
                let x = x0.floor() as i32;
                let w = (x1.ceil() as i32 - x).max(1);
                b.size_allocate(&gtk::Allocation::new(x, y, w, BAND_PX.min(height)), -1);
            }
        }
    }
}

/// Filete de limite ou barra de trecho: `strip > box.mark`, carmim.
fn mark_box() -> gtk::Widget {
    let m = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    m.add_css_class("mark");
    m.set_can_target(false);
    m.set_visible(false);
    m.upcast()
}

glib::wrapper! {
    /// Trilho com ate duas series, limites e trechos marcados. CSS: `strip`,
    /// `strip > line`, `strip > box.mark` (limite e trecho); `strip.alert`
    /// quando a ultima leitura passa do limite.
    pub struct Strip(ObjectSubclass<strip::Strip>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Strip {
    /// `series`: 1 (so a principal) ou 2.
    pub fn new(series: usize) -> Self {
        let s: Self = glib::Object::new();
        let imp = s.imp();
        let mut lines = Vec::new();
        for class in ["primary", "secondary"].iter().take(series) {
            let l = Line::new(class);
            // Antes da marca na ordem dos filhos: a marca de limite fica por cima.
            l.insert_before(&s, imp.marks.borrow().first());
            lines.push(l);
        }
        imp.lines.replace(lines);
        s
    }

    /// Valores de cada serie (na ordem do `new`), topo da escala e limite
    /// (valor na escala, ou `None`). Devolve se alguma serie repintou.
    pub fn set(&self, series: &[&[f32]], max: f64, cap: usize, limit: Option<f64>) -> bool {
        self.set_marked(series, max, cap, limit.as_slice(), &[])
    }

    /// [`Strip::set`] com varios limites (valores na escala; acima de `max`
    /// ficam de fora) e trechos marcados no rodape (`bands`: indices das
    /// amostras de `series[0]`, ver [`runs_at_least`]). Filetes e barras so
    /// nascem quando uma faixa pede mais do que ja tem; com o mesmo desenho,
    /// nada e realocado. Devolve se alguma serie repintou.
    pub fn set_marked(
        &self,
        series: &[&[f32]],
        max: f64,
        cap: usize,
        limits: &[f64],
        bands: &[Range<usize>],
    ) -> bool {
        let imp = self.imp();
        let mut changed = false;
        for (l, v) in imp.lines.borrow().iter().zip(series) {
            changed |= l.set(v, max, cap);
        }
        let mut relayout = false;
        let at: Vec<f64> = limits
            .iter()
            .filter(|l| **l <= max && max > 0.0)
            .map(|l| l / max)
            .collect();
        if *imp.limits.borrow() != at {
            let mut marks = imp.marks.borrow_mut();
            while marks.len() < at.len() {
                let m = mark_box();
                m.insert_before(self, None::<&gtk::Widget>);
                marks.push(m);
            }
            for (i, m) in marks.iter().enumerate() {
                m.set_visible(i < at.len());
            }
            imp.limits.replace(at);
            relayout = true;
        }
        let len = series.first().map_or(0, |s| s.len());
        let next = (bands.to_vec(), len, cap);
        if *imp.bands.borrow() != next {
            let mut boxes = imp.band_boxes.borrow_mut();
            while boxes.len() < bands.len() {
                let b = mark_box();
                b.add_css_class("band");
                b.insert_before(self, None::<&gtk::Widget>);
                boxes.push(b);
            }
            for (i, b) in boxes.iter().enumerate() {
                b.set_visible(i < bands.len());
            }
            imp.bands.replace(next);
            relayout = true;
        }
        if relayout {
            self.queue_allocate();
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentcenter_core::Ring;

    #[test]
    fn nice_max_rounds_up_to_1_2_5_or_powers_of_two() {
        assert_eq!(nice_max(140.0, 100.0, false), 200.0);
        assert_eq!(nice_max(23.0, 100.0, false), 100.0, "floor holds");
        assert_eq!(nice_max(501.0, 1.0, false), 1000.0);
        assert_eq!(nice_max(0.3, 0.1, false), 0.5);
        assert_eq!(nice_max(f64::NAN, 20.0, false), 20.0, "no reading: floor");
        let mib = 1024.0 * 1024.0;
        assert_eq!(nice_max(704.0 * mib, mib, true), 1024.0 * mib);
        assert_eq!(nice_max(1.2 * 1024.0 * mib, mib, true), 2048.0 * mib);
    }

    #[test]
    fn peak_skips_absent_samples() {
        let a = [1.0, f32::NAN, 7.0];
        let b = [3.0, 9.5];
        assert_eq!(peak([&a[..], &b[..]]), Some(9.5));
        assert_eq!(peak([&[f32::NAN][..]]), None);
        assert_eq!(peak(std::iter::empty::<&[f32]>()), None);
    }

    /// Criterio 2 do C19: o ring buffer vira pontos na escala, e depois da
    /// volta do buffer a mais antiga sai pela esquerda e a mais nova fica na
    /// borda direita.
    #[test]
    fn ring_to_points_scales_and_follows_the_wrap() {
        let mut r = Ring::new(5);
        for v in [10.0, 20.0, 40.0] {
            r.push(v);
        }
        // Incompleto: 3 de 5, encostado a direita; topo da escala em 40.
        let p = points(&r.to_vec(), r.capacity(), 40.0, 100.0, 40.0);
        assert_eq!(p, vec![vec![(50.0, 30.0), (75.0, 20.0), (100.0, 0.0)]]);

        // Volta: 7 pushes num ring de 5 descartam 10 e 20.
        for v in [0.0, 80.0, 30.0, 20.0] {
            r.push(v);
        }
        let v = r.to_vec();
        assert_eq!(v, [40.0, 0.0, 80.0, 30.0, 20.0]);
        let p = points(&v, r.capacity(), 80.0, 100.0, 40.0);
        assert_eq!(
            p,
            vec![vec![
                (0.0, 20.0),
                (25.0, 40.0),
                (50.0, 0.0),
                (75.0, 25.0),
                (100.0, 30.0)
            ]]
        );
        // Escala menor que o valor: preso no topo, nunca fora do retangulo.
        let p = points(&v, r.capacity(), 40.0, 100.0, 40.0);
        assert_eq!(p[0][2], (50.0, 0.0));
        assert!(p[0].iter().all(|(_, y)| (0.0..=40.0).contains(y)));
    }

    #[test]
    fn ring_of_600_maps_the_newest_to_the_right_edge_after_many_laps() {
        let mut r = Ring::new(HISTORY_LEN);
        for i in 0..(HISTORY_LEN * 3 + 7) {
            r.push(i as f32);
        }
        let v = r.to_vec();
        let max = nice_max(peak([&v[..]]).unwrap(), 1.0, false);
        let p = points(&v, r.capacity(), max, 599.0, 50.0);
        assert_eq!(p.len(), 1);
        let run = &p[0];
        assert_eq!(run.len(), HISTORY_LEN);
        assert_eq!(run[0].0, 0.0);
        assert_eq!(run[HISTORY_LEN - 1].0, 599.0);
        // Cronologico: x cresce 1 px por amostra e y so sobe (valor so cresce).
        assert!(
            run.windows(2)
                .all(|w| w[1].0 - w[0].0 == 1.0 && w[1].1 <= w[0].1)
        );
    }

    #[test]
    fn absent_samples_break_the_line_and_never_become_zero() {
        let v = [1.0, f32::NAN, f32::NAN, 2.0, 3.0, f32::NAN];
        let p = points(&v, 6, 4.0, 50.0, 8.0);
        assert_eq!(p, vec![vec![(0.0, 6.0)], vec![(30.0, 4.0), (40.0, 2.0)]]);
        assert!(points(&[f32::NAN; 4], 4, 1.0, 10.0, 10.0).is_empty());
        assert!(points(&[], HISTORY_LEN, 1.0, 10.0, 10.0).is_empty());
    }

    #[test]
    fn equal_series_with_absent_samples_count_as_unchanged() {
        assert!(same(&[1.0, f32::NAN], &[1.0, f32::NAN]));
        assert!(!same(&[1.0, f32::NAN], &[1.0, 0.0]));
        assert!(!same(&[1.0], &[1.0, 1.0]));
    }

    #[test]
    fn runs_at_least_breaks_on_gaps_and_values_below() {
        let v = [0.0, 3.0, 5.0, f32::NAN, 4.0, 2.0, 3.0, 3.0];
        assert_eq!(runs_at_least(&v, 3.0), vec![1..3, 4..5, 6..8]);
        assert!(
            runs_at_least(&[f32::NAN; 3], 0.0).is_empty(),
            "lacuna nao e trecho"
        );
        assert!(runs_at_least(&[], 3.0).is_empty());
    }

    /// As barras ficam sob os mesmos x de [`points`]: meia amostra para cada
    /// lado, encostadas a direita com o ring incompleto, presas em `[0, w]`.
    // Um trecho so e o caso testado: a lista de um `Range`, nao uma faixa de 0 a 2.
    #[allow(clippy::single_range_in_vec_init)]
    #[test]
    fn band_spans_follow_the_points_and_stay_inside() {
        // 5 amostras de 5 num trilho de 100 px: passo 25.
        let v = [0.0, 3.0, 3.0, 0.0, 3.0];
        let runs = runs_at_least(&v, 3.0);
        let p = points(&v, 5, 3.0, 100.0, 10.0);
        let xs: Vec<f32> = p[0].iter().map(|(x, _)| *x).collect();
        let spans = band_spans(&runs, 5, 5, 100.0);
        assert_eq!(spans, vec![(12.5, 62.5), (87.5, 100.0)]);
        assert!(spans[0].0 < xs[1] && xs[2] < spans[0].1);
        // Incompleto: 2 de 5 encostadas a direita (x 75 e 100).
        let spans = band_spans(&[0..2], 2, 5, 100.0);
        assert_eq!(spans, vec![(62.5, 100.0)]);
        // Uma amostra num ring de 600 em 300 px: ainda BAND_MIN_PX de largura.
        let spans = band_spans(&[10..11], HISTORY_LEN, HISTORY_LEN, 300.0);
        assert!(
            (spans[0].1 - spans[0].0 - BAND_MIN_PX).abs() < 1e-4,
            "{spans:?}"
        );
        // Trecho fora da serie e ignorado, nunca desenhado fora.
        assert!(band_spans(&[3..9], 4, 4, 100.0).is_empty());
        assert!(band_spans(&[0..0], 4, 4, 100.0).is_empty());
    }

    #[test]
    fn mark_y_places_limits_on_the_scale() {
        // 33 px de altura, 1,5 de folga: 31,5 uteis.
        assert_eq!(mark_y(0.0, 33), 32, "zero no fundo");
        assert_eq!(mark_y(1.0, 33), 0, "topo");
        assert_eq!(mark_y(0.7, 33), 9);
        assert_eq!(mark_y(0.9, 33), 3);
        assert!(mark_y(0.9, 33) < mark_y(0.7, 33), "90 % acima de 70 %");
        assert_eq!(mark_y(2.0, 33), 0, "acima da escala fica no topo");
        assert_eq!(mark_y(0.5, 0), 0, "sem altura nao sai do trilho");
    }
}

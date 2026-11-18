//! Contrast gate (V1.3.b). Reads the CSS files as text, so it runs with no
//! display and no GTK init, and fails when a pair drops below WCAG 2.x:
//! text 4.5:1, chart and state marks 3:1, in the light, dark and
//! high-contrast themes. `scripts/contrast.py` measures the token layer too;
//! this test also covers the literals and opacities it cannot see (row rank,
//! sparkline, heat palette) and the rule that state is never colour only.

use std::collections::BTreeMap;
use std::path::PathBuf;

const TEXT_MIN: f64 = 4.5;
const MARK_MIN: f64 = 3.0;

type Env = BTreeMap<String, String>;

fn res(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn strip_comments(css: &str) -> String {
    let mut out = String::new();
    let mut rest = css;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        rest = match rest[i..].find("*/") {
            Some(j) => &rest[i + j + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Top-level text, and each `@media (cond) { body }` as (cond, body).
fn segments(css: &str) -> (String, Vec<(String, String)>) {
    let css = strip_comments(css);
    let (mut top, mut media) = (String::new(), Vec::new());
    let mut rest = css.as_str();
    while let Some(i) = rest.find("@media") {
        top.push_str(&rest[..i]);
        let after = &rest[i + 6..];
        let open = after.find('{').expect("@media without a block");
        let cond = after[..open].trim().to_string();
        let (mut depth, mut end) = (1, open + 1);
        for (k, ch) in after[open + 1..].char_indices() {
            depth += match ch {
                '{' => 1,
                '}' => -1,
                _ => 0,
            };
            if depth == 0 {
                end = open + 1 + k;
                break;
            }
        }
        media.push((cond, after[open + 1..end].to_string()));
        rest = &after[(end + 1).min(after.len())..];
    }
    top.push_str(rest);
    (top, media)
}

fn decls(body: &str) -> Vec<(String, String)> {
    body.split(';')
        .filter_map(|d| {
            let (k, v) = d.split_once(':')?;
            let k = k.trim().rsplit(|c: char| c.is_whitespace() || c == '{').next()?;
            k.starts_with("--")
                .then(|| (k.to_string(), v.trim().to_string()))
        })
        .collect()
}

fn root_vars(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(i) = rest.find(":root") {
        let after = &rest[i..];
        let open = after.find('{').expect(":root without a block");
        let close = after[open..].find('}').expect(":root never closes") + open;
        out.extend(decls(&after[open + 1..close]));
        rest = &after[close..];
    }
    out
}

/// The themes the gate measures. High contrast is the base theme plus any
/// `prefers-contrast: more` override (GTK turns it on from AdwStyleManager).
fn themes(tokens_css: &str) -> Vec<(&'static str, Env)> {
    let (top, media) = segments(tokens_css);
    let light: Env = root_vars(&top).into_iter().collect();
    let pick = |needle: &str| -> Vec<(String, String)> {
        media
            .iter()
            .filter(|(c, _)| c.contains(needle))
            .flat_map(|(_, b)| root_vars(b))
            .collect()
    };
    let (dark_over, hc_over) = (pick("prefers-color-scheme: dark"), pick("prefers-contrast: more"));
    let mut dark = light.clone();
    dark.extend(dark_over);
    let (mut hc_light, mut hc_dark) = (light.clone(), dark.clone());
    hc_light.extend(hc_over.clone());
    hc_dark.extend(hc_over);
    vec![
        ("light", light),
        ("dark", dark),
        ("high-contrast-light", hc_light),
        ("high-contrast-dark", hc_dark),
    ]
}

fn resolve(name: &str, env: &Env, depth: u32) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let v = env.get(name)?;
    match v.strip_prefix("var(").and_then(|s| s.strip_suffix(')')) {
        Some(inner) => resolve(inner.trim(), env, depth + 1),
        None => Some(v.clone()),
    }
}

type Rgba = (f64, f64, f64, f64);

fn hex(v: &str) -> Option<Rgba> {
    let h = v.trim().strip_prefix('#')?;
    let h: String = match h.len() {
        3 | 4 => h.chars().flat_map(|c| [c, c]).collect(),
        6 | 8 => h.to_string(),
        _ => return None,
    };
    let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map(|x| f64::from(x) / 255.0);
    let a = if h.len() == 8 { b(6).ok()? } else { 1.0 };
    Some((b(0).ok()?, b(2).ok()?, b(4).ok()?, a))
}

fn lum(c: Rgba) -> f64 {
    let ch = |x: f64| if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) };
    0.2126 * ch(c.0) + 0.7152 * ch(c.1) + 0.0722 * ch(c.2)
}

fn ratio(fg: Rgba, bg: Rgba) -> f64 {
    let a = fg.3;
    let fg = (
        fg.0 * a + bg.0 * (1.0 - a),
        fg.1 * a + bg.1 * (1.0 - a),
        fg.2 * a + bg.2 * (1.0 - a),
        1.0,
    );
    let (x, y) = (lum(fg), lum(bg));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// Every token pair the naming scheme promises, as failures of the minimum.
fn token_failures(env: &Env) -> (usize, Vec<String>) {
    let colors: BTreeMap<&str, Rgba> = env
        .keys()
        .filter_map(|n| Some((n.as_str(), hex(&resolve(n, env, 0)?)?)))
        .collect();
    let surfaces: Vec<&str> = colors.keys().copied().filter(|n| n.starts_with("--ac-surface-")).collect();
    let mut pairs: Vec<(String, String, f64, Rgba)> = Vec::new();
    for (&n, &c) in &colors {
        let on_sel = ["--ac-text-rec-", "--ac-mark-rec-", "--ac-mark-focus"].iter().any(|p| n.starts_with(p));
        let mut bgs: Vec<&str> = surfaces.clone();
        if on_sel {
            bgs.push("--ac-selection");
        }
        if n.starts_with("--ac-text-large-") {
            pairs.extend(surfaces.iter().map(|s| (n.into(), (*s).into(), MARK_MIN, c)));
        } else if n.starts_with("--ac-text-") {
            pairs.extend(bgs.iter().map(|s| (n.into(), (*s).into(), TEXT_MIN, c)));
        } else if n.starts_with("--ac-mark-") {
            bgs.push("--ac-track");
            pairs.extend(bgs.iter().map(|s| (n.into(), (*s).into(), MARK_MIN, c)));
        } else if let Some(rest) = n.strip_prefix("--ac-on-") {
            if let Some((base, _)) = rest.rsplit_once('-') {
                pairs.push((n.into(), format!("--ac-{base}"), TEXT_MIN, c));
            }
        }
    }
    if let (Some(dim), Some(&(r, g, b, _))) = (env.get("--dim-opacity"), colors.get("--ac-text-primary")) {
        let a: f64 = dim.trim_end_matches('%').parse::<f64>().unwrap() / 100.0;
        pairs.extend(surfaces.iter().map(|s| ("--dim-opacity".into(), (*s).into(), TEXT_MIN, (r, g, b, a))));
    }
    let mut bad = Vec::new();
    for (fg, bg, need, c) in &pairs {
        match colors.get(bg.as_str()) {
            None => bad.push(format!("{fg} on {bg}: background unresolved")),
            Some(&b) => {
                let r = ratio(*c, b);
                if r < *need {
                    bad.push(format!("{fg} on {bg}: {r:.2}:1 < {need}"));
                }
            }
        }
    }
    (pairs.len(), bad)
}

/// Value of `prop` in the first rule whose selector is exactly `selector`.
fn rule_prop(src: &str, selector: &str, prop: &str) -> Option<String> {
    let needle = format!("{selector} {{");
    let start = src.find(&needle)? + needle.len();
    let body = &src[start..src[start..].find('}')? + start];
    body.split(';').find_map(|d| {
        let (k, v) = d.split_once(':')?;
        (k.trim() == prop).then(|| v.trim().to_string())
    })
}

fn surfaces_of(env: &Env) -> Vec<(String, Rgba)> {
    let mut v: Vec<String> = env.keys().filter(|n| n.starts_with("--ac-surface-")).cloned().collect();
    v.push("--ac-selection".into());
    v.into_iter()
        .map(|n| {
            let c = hex(&resolve(&n, env, 0).unwrap()).unwrap();
            (n, c)
        })
        .collect()
}

#[test]
fn ratio_matches_the_wcag_reference() {
    let (black, white) = ((0.0, 0.0, 0.0, 1.0), (1.0, 1.0, 1.0, 1.0));
    assert!((ratio(black, white) - 21.0).abs() < 1e-9);
    assert!((ratio(white, white) - 1.0).abs() < 1e-9);
    // #777 on white is the textbook 4.48:1, just under the text minimum.
    assert!((ratio(hex("#777777").unwrap(), white) - 4.48).abs() < 0.01);
}

#[test]
fn the_checker_fails_below_the_minimums() {
    let css = ":root { --ac-surface-window: #ffffff; --ac-track: #ffffff; --ac-text-x: #777777; --ac-mark-x: #aaaaaa; --ac-text-ok: #595959; }";
    let (_, bad) = token_failures(&themes(css)[0].1);
    assert_eq!(bad.len(), 3, "{bad:?}");
    assert!(bad.iter().any(|b| b.starts_with("--ac-text-x on --ac-surface-window")));
    assert!(bad.iter().any(|b| b.starts_with("--ac-mark-x on --ac-surface-window")));
}

#[test]
fn themes_cover_light_dark_and_high_contrast() {
    let t = themes(&res("resources/tokens.css"));
    let names: Vec<_> = t.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, ["light", "dark", "high-contrast-light", "high-contrast-dark"]);
    assert_ne!(t[0].1["--ac-surface-view"], t[1].1["--ac-surface-view"], "dark block not read");
}

#[test]
fn tokens_meet_wcag_in_every_theme() {
    for (theme, env) in themes(&res("resources/tokens.css")) {
        let (n, bad) = token_failures(&env);
        assert!(n > 80, "{theme}: only {n} pairs measured");
        assert!(bad.is_empty(), "{theme}: {bad:#?}");
    }
}

/// The rank is `--ac-text-primary` at the row-rank opacity, on every row
/// background including the selected one.
#[test]
fn row_rank_text_meets_4_5_on_every_row_background() {
    let css = res("resources/sessions.css");
    let (top, _) = segments(&css);
    let a: f64 = rule_prop(&top, "listview.session-rack .row-rank", "opacity")
        .expect("row-rank opacity")
        .parse()
        .unwrap();
    for (theme, env) in themes(&res("resources/tokens.css")) {
        let (r, g, b, _) = hex(&resolve("--ac-text-primary", &env, 0).unwrap()).unwrap();
        for (name, bg) in surfaces_of(&env) {
            let got = ratio((r, g, b, a), bg);
            assert!(got >= TEXT_MIN, "{theme}: rank at {a} on {name}: {got:.2}:1");
        }
    }
}

/// The sparkline is a chart mark with its own literal per theme.
#[test]
fn sparkline_mark_meets_3_to_1_on_every_row_background() {
    let css = res("resources/sessions.css");
    let (top, media) = segments(&css);
    let dark_body = media
        .iter()
        .find(|(c, _)| c.contains("prefers-color-scheme: dark"))
        .map(|(_, b)| b.clone())
        .expect("dark spark override");
    let sel = "listview.session-rack .row-spark";
    let light_c = rule_prop(&top, sel, "color").unwrap();
    let dark_c = rule_prop(&dark_body, sel, "color").unwrap();
    for (theme, env) in themes(&res("resources/tokens.css")) {
        let spark = hex(if theme.contains("dark") { &dark_c } else { &light_c }).unwrap();
        for (name, bg) in surfaces_of(&env) {
            let got = ratio(spark, bg);
            assert!(got >= MARK_MIN, "{theme}: spark on {name}: {got:.2}:1");
        }
    }
}

/// Token map heat: on-fill text 4.5:1 on its fill, the edge 3:1 on the panel
/// and the view.
#[test]
fn heat_palette_meets_wcag_in_both_schemes() {
    let css = strip_comments(&res("resources/token_map.css"));
    let (_, media) = segments(&css);
    let tokens = themes(&res("resources/tokens.css"));
    for (scheme, env) in [("light", &tokens[0].1), ("dark", &tokens[1].1)] {
        let body = media
            .iter()
            .find(|(c, _)| c.contains(&format!("prefers-color-scheme: {scheme}")))
            .unwrap_or_else(|| panic!("no {scheme} heat block"))
            .1
            .clone();
        let defs: BTreeMap<String, Rgba> = body
            .split(';')
            .filter_map(|d| {
                let d = d.trim().strip_prefix("@define-color")?;
                let (n, v) = d.trim().split_once(char::is_whitespace)?;
                Some((n.to_string(), hex(v)?))
            })
            .collect();
        for i in 0..5 {
            let (fill, text) = (defs[&format!("ac_heat_{i}")], defs[&format!("ac_heat_{i}_text")]);
            let got = ratio(text, fill);
            assert!(got >= TEXT_MIN, "{scheme}: heat {i} text {got:.2}:1");
        }
        for (name, bg) in surfaces_of(env).iter().filter(|(n, _)| n.ends_with("panel") || n.ends_with("view")) {
            let got = ratio(defs["ac_heat_edge"], *bg);
            assert!(got >= MARK_MIN, "{scheme}: heat edge on {name}: {got:.2}:1");
        }
    }
}

/// `Group::X => (Shape::Y, "tone")` arms of `state_glyph::glyph`.
fn glyph_table() -> Vec<(String, String, String)> {
    res("src/state_glyph.rs")
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let (g, rest) = l.strip_prefix("Group::")?.split_once("=>")?;
            let rest = rest.trim().strip_prefix("(Shape::")?;
            let (shape, tone) = rest.split_once(',')?;
            Some((g.trim().into(), shape.into(), tone.trim().trim_end_matches([')', ',']).trim_matches('"').into()))
        })
        .collect()
}

fn group_variants() -> Vec<String> {
    let src = res("src/format.rs");
    let body = src.split("pub enum Group {").nth(1).expect("enum Group").split('}').next().unwrap();
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.trim_end_matches(',').to_string())
        .collect()
}

/// State is never colour only: every session state has a glyph, the four
/// states that carry information have four different shapes, and each tone
/// class the glyph names has a rule that colours it.
#[test]
fn every_state_has_a_glyph_shape_besides_colour() {
    let table = glyph_table();
    let variants = group_variants();
    assert_eq!(variants.len(), 5, "Group changed: {variants:?}");
    for v in &variants {
        assert!(table.iter().any(|(g, ..)| g == v), "state {v} has no glyph arm");
    }
    let shapes: Vec<&str> = table.iter().filter(|(g, ..)| g != "Unknown").map(|(_, s, _)| s.as_str()).collect();
    let mut uniq = shapes.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(uniq.len(), shapes.len(), "two states share a shape: {table:?}");
    let css = res("resources/sessions.css");
    for (g, _, tone) in &table {
        assert!(css.contains(&format!("glyph.{tone} {{")), "state {g}: no rule for glyph.{tone}");
    }
}

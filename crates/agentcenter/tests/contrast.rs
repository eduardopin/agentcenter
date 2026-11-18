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
            let k = k
                .trim()
                .rsplit(|c: char| c.is_whitespace() || c == '{')
                .next()?;
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

/// The media features of one `@media` condition, whitespace-free and
/// lower-case, so `(prefers-color-scheme: dark)` matches exactly and
/// `(prefers-color-scheme: darkx)` or an extra feature does not.
fn feats(cond: &str) -> Vec<String> {
    let mut v: Vec<String> = cond
        .to_ascii_lowercase()
        .split(" and ")
        .map(|f| f.split_whitespace().collect::<String>())
        .collect();
    v.sort();
    v
}

const DARK: &str = "(prefers-color-scheme:dark)";
const MORE: &str = "(prefers-contrast:more)";

/// The media blocks whose condition is exactly this set of features.
fn blocks<'a>(media: &'a [(String, String)], want: &[&str]) -> Vec<&'a str> {
    let mut want: Vec<String> = want.iter().map(|s| (*s).to_string()).collect();
    want.sort();
    media
        .iter()
        .filter(|(c, _)| feats(c) == want)
        .map(|(_, b)| b.as_str())
        .collect()
}

/// The themes the gate measures. High contrast in this app is the base
/// tokens plus the system override: GTK/libadwaita turn `prefers-contrast:
/// more` on from the OS setting and restyle their own widgets, and the app
/// leans on that (see the `--dim-opacity` note in tokens.css, "sobram para o
/// contraste alto do sistema"). tokens.css has no `prefers-contrast` block
/// today, so the HC themes equal the base ones; `high_contrast_is_the_base_until_a_block_exists`
/// pins that and measures any block that appears.
fn themes(tokens_css: &str) -> Vec<(&'static str, Env)> {
    let (top, media) = segments(tokens_css);
    let light: Env = root_vars(&top).into_iter().collect();
    let over = |want: &[&str]| -> Vec<(String, String)> {
        blocks(&media, want)
            .into_iter()
            .flat_map(root_vars)
            .collect()
    };
    let mut dark = light.clone();
    dark.extend(over(&[DARK]));
    let mut hc_light = light.clone();
    hc_light.extend(over(&[MORE]));
    let mut hc_dark = dark.clone();
    hc_dark.extend(over(&[MORE]));
    hc_dark.extend(over(&[DARK, MORE]));
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
    let ch = |x: f64| {
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
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

/// Tokens the naming scheme treats as colours: the ones token_failures pairs.
fn is_colour_token(n: &str) -> bool {
    [
        "--ac-surface-",
        "--ac-text-",
        "--ac-mark-",
        "--ac-on-",
        "--ac-selection",
        "--ac-track",
    ]
    .iter()
    .any(|p| n.starts_with(p))
}

/// Every token pair the naming scheme promises, as failures of the minimum.
fn token_failures(env: &Env) -> (usize, Vec<String>) {
    let colors: BTreeMap<&str, Rgba> = env
        .keys()
        .filter_map(|n| Some((n.as_str(), hex(&resolve(n, env, 0)?)?)))
        .collect();
    // A colour token the checker cannot read must fail, not vanish from the
    // pairs: rgb(), hsl(), a name or a broken var() chain.
    let mut unmeasured: Vec<String> = env
        .keys()
        .filter(|n| is_colour_token(n) && !colors.contains_key(n.as_str()))
        .map(|n| {
            format!(
                "{n}: {:?} is a colour the gate cannot measure (use a #hex or a var() to one)",
                resolve(n, env, 0)
            )
        })
        .collect();
    let surfaces: Vec<&str> = colors
        .keys()
        .copied()
        .filter(|n| n.starts_with("--ac-surface-"))
        .collect();
    let mut pairs: Vec<(String, String, f64, Rgba)> = Vec::new();
    for (&n, &c) in &colors {
        let on_sel = ["--ac-text-rec-", "--ac-mark-rec-", "--ac-mark-focus"]
            .iter()
            .any(|p| n.starts_with(p));
        let mut bgs: Vec<&str> = surfaces.clone();
        if on_sel {
            bgs.push("--ac-selection");
        }
        if n.starts_with("--ac-text-large-") {
            pairs.extend(
                surfaces
                    .iter()
                    .map(|s| (n.into(), (*s).into(), MARK_MIN, c)),
            );
        } else if n.starts_with("--ac-text-") {
            pairs.extend(bgs.iter().map(|s| (n.into(), (*s).into(), TEXT_MIN, c)));
        } else if n.starts_with("--ac-mark-") {
            bgs.push("--ac-track");
            pairs.extend(bgs.iter().map(|s| (n.into(), (*s).into(), MARK_MIN, c)));
        } else if let Some(rest) = n.strip_prefix("--ac-on-")
            && let Some((base, _)) = rest.rsplit_once('-')
        {
            pairs.push((n.into(), format!("--ac-{base}"), TEXT_MIN, c));
        }
    }
    if let (Some(dim), Some(&(r, g, b, _))) =
        (env.get("--dim-opacity"), colors.get("--ac-text-primary"))
    {
        let a: f64 = dim.trim_end_matches('%').parse::<f64>().unwrap() / 100.0;
        pairs.extend(
            surfaces
                .iter()
                .map(|s| ("--dim-opacity".into(), (*s).into(), TEXT_MIN, (r, g, b, a))),
        );
    }
    let mut bad = Vec::new();
    bad.append(&mut unmeasured);
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
    let mut v: Vec<String> = env
        .keys()
        .filter(|n| n.starts_with("--ac-surface-"))
        .cloned()
        .collect();
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
    assert!(
        bad.iter()
            .any(|b| b.starts_with("--ac-text-x on --ac-surface-window"))
    );
    assert!(
        bad.iter()
            .any(|b| b.starts_with("--ac-mark-x on --ac-surface-window"))
    );
}

#[test]
fn themes_cover_light_dark_and_high_contrast() {
    let t = themes(&res("resources/tokens.css"));
    let names: Vec<_> = t.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        names,
        ["light", "dark", "high-contrast-light", "high-contrast-dark"]
    );
    assert_ne!(
        t[0].1["--ac-surface-view"], t[1].1["--ac-surface-view"],
        "dark block not read"
    );
}

/// The media match is exact: a near-miss condition must not be read as dark
/// or as high contrast.
#[test]
fn media_conditions_match_exactly() {
    let css = |c: &str| {
        format!(
            ":root {{ --ac-surface-view: #ffffff; }} @media {c} {{ :root {{ --ac-surface-view: #000000; }} }}"
        )
    };
    let view = |c: &str, i: usize| themes(&css(c))[i].1["--ac-surface-view"].clone();
    assert_eq!(view("(prefers-color-scheme: dark)", 1), "#000000");
    assert_eq!(view("(prefers-color-scheme:dark)", 1), "#000000");
    for near in [
        "(prefers-color-scheme: darkx)",
        "(prefers-color-scheme: dark) and (min-width: 1px)",
        "(prefers-color-scheme: light)",
    ] {
        assert_eq!(view(near, 1), "#ffffff", "{near} read as dark");
    }
    assert_eq!(view("(prefers-contrast: more)", 2), "#000000");
    assert_eq!(view("(prefers-contrast: moreish)", 2), "#ffffff");
}

/// Unmeasurable colour tokens fail instead of being skipped.
#[test]
fn the_checker_fails_on_colours_it_cannot_measure() {
    for v in [
        "rgb(1, 2, 3)",
        "rgba(0,0,0,.5)",
        "hsl(10 20% 30%)",
        "tomato",
        "var(--missing)",
    ] {
        let css = format!(
            ":root {{ --ac-surface-window: #ffffff; --ac-track: #ffffff; --ac-text-x: {v}; }}"
        );
        let (_, bad) = token_failures(&themes(&css)[0].1);
        assert!(
            bad.iter()
                .any(|b| b.starts_with("--ac-text-x") && b.contains("cannot measure")),
            "{v}: {bad:?}"
        );
    }
}

/// High contrast here is the base tokens plus the system override. Until
/// tokens.css grows a `prefers-contrast: more` block the HC themes are the
/// base ones, and this says so; the day a block lands it is measured by
/// `tokens_meet_wcag_in_every_theme` and must change at least one token.
#[test]
fn high_contrast_is_the_base_until_a_block_exists() {
    let css = res("resources/tokens.css");
    let (_, media) = segments(&css);
    let has_block = media
        .iter()
        .any(|(c, _)| feats(c).iter().any(|f| f.contains("prefers-contrast")));
    let t = themes(&css);
    if has_block {
        assert!(
            t[2].1 != t[0].1 || t[3].1 != t[1].1,
            "a prefers-contrast block changes no token"
        );
    } else {
        assert_eq!(
            t[2].1, t[0].1,
            "HC light differs from light without a block"
        );
        assert_eq!(t[3].1, t[1].1, "HC dark differs from dark without a block");
    }
}

/// Every CSS file in resources/ other than tokens.css (the measured token
/// layer) may carry a literal colour only where the gate measures it: the
/// session row sparkline and the token map heat palette. Anything else, in
/// any file, fails until it is a token or measured here.
#[test]
fn no_unmeasured_literal_colour_in_any_stylesheet() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "css"))
        .collect();
    files.sort();
    assert!(files.len() >= 8, "resources/*.css not found: {files:?}");
    let sessions = res("resources/sessions.css");
    let (top, media) = segments(&sessions);
    let spark = "listview.session-rack .row-spark";
    let measured_spark: Vec<String> = std::iter::once(rule_prop(&top, spark, "color").unwrap())
        .chain(
            media
                .iter()
                .filter_map(|(_, b)| rule_prop(b, spark, "color")),
        )
        .collect();
    let mut stray = Vec::new();
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        if name == "tokens.css" {
            continue;
        }
        for (line, value) in literal_colours(&strip_comments(&std::fs::read_to_string(&f).unwrap()))
        {
            let ok = (name == "token_map.css"
                && line.trim_start().starts_with("@define-color")
                && hex(&value).is_some())
                || (name == "sessions.css" && measured_spark.contains(&value));
            if !ok {
                stray.push(format!("{name}: {value} in `{}`", line.trim()));
            }
        }
    }
    assert!(
        stray.is_empty(),
        "literal colours the gate does not measure: {stray:#?}"
    );
}

/// The detector itself: it finds hex, functions and colour names.
#[test]
fn the_literal_scan_finds_every_colour_form() {
    let css = "a { color: #abc; background: rgba(0,0,0,.5); border-color: tomato; fill: alpha(red, .5); outline-color: var(--x); color: currentColor; outline: 2px solid #f00; border-top-color: navy; outline: 1px solid var(--y); }\n@define-color n #123456;";
    let got: Vec<String> = literal_colours(css).into_iter().map(|(_, v)| v).collect();
    assert_eq!(
        got,
        [
            "#abc",
            "rgba(0,0,0,.5)",
            "tomato",
            "alpha(red, .5)",
            "#f00",
            "navy",
            "#123456"
        ]
    );
}

/// Colour values written literally in `css`, as (line, value). Looks at the
/// colour properties and `@define-color`; a `var()`, `inherit`, `none`,
/// `transparent` or `currentColor` is not a literal.
fn literal_colours(css: &str) -> Vec<(String, String)> {
    const PROPS: [&str; 21] = [
        "color",
        "background",
        "background-color",
        "background-image",
        "border-color",
        "border-top-color",
        "border-right-color",
        "border-bottom-color",
        "border-left-color",
        "outline-color",
        "text-decoration-color",
        "fill",
        "stroke",
        "caret-color",
        "text-shadow",
        "box-shadow",
        "-gtk-icon-shadow",
        "-gtk-icon-palette",
        "border",
        "outline",
        "text-decoration",
    ];
    // Shorthands mix a colour with keywords (`1px solid`), so only a hex or a
    // colour function counts there, never a bare word.
    const SHORTHAND: [&str; 8] = [
        "border",
        "outline",
        "box-shadow",
        "text-shadow",
        "-gtk-icon-shadow",
        "-gtk-icon-palette",
        "text-decoration",
        "background-image",
    ];
    const NOT: [&str; 6] = [
        "inherit",
        "none",
        "transparent",
        "currentcolor",
        "initial",
        "unset",
    ];
    let mut out = Vec::new();
    for line in css.lines() {
        if let Some(rest) = line.trim().strip_prefix("@define-color") {
            if let Some((_, v)) = rest.trim().split_once(char::is_whitespace) {
                out.push((line.to_string(), v.trim().trim_end_matches(';').to_string()));
            }
            continue;
        }
        for decl in line.split(';') {
            let Some((k, v)) = decl
                .rsplit_once('{')
                .map_or(decl, |(_, d)| d)
                .split_once(':')
            else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim().trim_end_matches('}').trim());
            if !PROPS.contains(&k) || v.is_empty() || v.starts_with("var(") {
                continue;
            }
            // Split on spaces outside parentheses so `1px solid #fff` and `alpha(red, .5)` both work.
            let (mut depth, mut cur, mut parts) = (0, String::new(), Vec::new());
            for ch in v.chars() {
                match ch {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                if ch.is_whitespace() && depth == 0 {
                    parts.push(std::mem::take(&mut cur));
                } else {
                    cur.push(ch);
                }
            }
            parts.push(cur);
            for p in parts.into_iter().filter(|p| !p.is_empty()) {
                let l = p.to_ascii_lowercase();
                // A colour function over a token or currentColor inherits a measured colour.
                let derived = l.contains("var(") || l.contains("currentcolor") || l.contains('@');
                let func = [
                    "rgb(", "rgba(", "hsl(", "hsla(", "alpha(", "shade(", "mix(", "lighter(",
                    "darker(",
                ]
                .iter()
                .any(|f| l.starts_with(f))
                    && !derived;
                let is_literal = p.starts_with('#')
                    || func
                    || (!SHORTHAND.contains(&k)
                        && !NOT.contains(&l.as_str())
                        && !p.starts_with('@')
                        && !p.starts_with("var(")
                        && p.chars().all(|c| c.is_ascii_alphabetic()));
                if is_literal {
                    out.push((line.to_string(), p));
                }
            }
        }
    }
    out
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
            assert!(
                got >= TEXT_MIN,
                "{theme}: rank at {a} on {name}: {got:.2}:1"
            );
        }
    }
}

/// The sparkline is a chart mark with its own literal per theme.
#[test]
fn sparkline_mark_meets_3_to_1_on_every_row_background() {
    let css = res("resources/sessions.css");
    let (top, media) = segments(&css);
    let dark_body = blocks(&media, &[DARK])
        .first()
        .map(|b| (*b).to_string())
        .expect("dark spark override");
    let sel = "listview.session-rack .row-spark";
    let light_c = rule_prop(&top, sel, "color").unwrap();
    let dark_c = rule_prop(&dark_body, sel, "color").unwrap();
    for (theme, env) in themes(&res("resources/tokens.css")) {
        let spark = hex(if theme.contains("dark") {
            &dark_c
        } else {
            &light_c
        })
        .unwrap();
        for (name, bg) in surfaces_of(&env) {
            let got = ratio(spark, bg);
            assert!(got >= MARK_MIN, "{theme}: spark on {name}: {got:.2}:1");
        }
    }
}

/// Token map heat: on-fill text 4.5:1 on its fill, the edge 3:1 on the panel
/// and the view.
#[test]
fn heat_palette_meets_wcag_in_every_theme() {
    let css = strip_comments(&res("resources/token_map.css"));
    let (_, media) = segments(&css);
    let defs_of = |body: &str| -> BTreeMap<String, Rgba> {
        body.split(';')
            .filter_map(|d| {
                let d = d.trim().strip_prefix("@define-color")?;
                let (n, v) = d.trim().split_once(char::is_whitespace)?;
                Some((n.to_string(), hex(v)?))
            })
            .collect()
    };
    // High contrast is the scheme's palette plus any `prefers-contrast: more`
    // block; token_map.css has none today, so it measures the same colours
    // against the HC surfaces.
    let has_hc = media
        .iter()
        .any(|(c, _)| feats(c).iter().any(|f| f.contains("prefers-contrast")));
    assert!(
        !has_hc || !blocks(&media, &[MORE]).is_empty() || !blocks(&media, &[DARK, MORE]).is_empty(),
        "unrecognised prefers-contrast block in token_map.css"
    );
    let mut n = 0;
    for (theme, env) in themes(&res("resources/tokens.css")) {
        let dark = theme.ends_with("dark");
        let mut defs = defs_of(
            blocks(
                &media,
                &[if dark {
                    DARK
                } else {
                    "(prefers-color-scheme:light)"
                }],
            )
            .first()
            .copied()
            .unwrap_or_else(|| {
                if dark {
                    panic!("no dark heat block")
                } else {
                    ""
                }
            }),
        );
        if defs.is_empty() {
            // Light is the unconditional palette outside any media block.
            let (top, _) = segments(&css);
            defs = defs_of(&top);
        }
        if theme.starts_with("high-contrast") {
            for b in blocks(&media, &[MORE]).into_iter().chain(if dark {
                blocks(&media, &[DARK, MORE])
            } else {
                vec![]
            }) {
                defs.extend(defs_of(b));
            }
        }
        for i in 0..5 {
            let (fill, text) = (
                defs[&format!("ac_heat_{i}")],
                defs[&format!("ac_heat_{i}_text")],
            );
            let got = ratio(text, fill);
            assert!(got >= TEXT_MIN, "{theme}: heat {i} text {got:.2}:1");
            n += 1;
        }
        for (name, bg) in surfaces_of(&env)
            .iter()
            .filter(|(nm, _)| nm.ends_with("panel") || nm.ends_with("view"))
        {
            let got = ratio(defs["ac_heat_edge"], *bg);
            assert!(got >= MARK_MIN, "{theme}: heat edge on {name}: {got:.2}:1");
            n += 1;
        }
    }
    assert_eq!(n, 4 * (5 + 2), "heat pairs measured");
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
            Some((
                g.trim().into(),
                shape.into(),
                tone.trim()
                    .trim_end_matches([')', ','])
                    .trim_matches('"')
                    .into(),
            ))
        })
        .collect()
}

fn group_variants() -> Vec<String> {
    let src = res("src/format.rs");
    let body = src
        .split("pub enum Group {")
        .nth(1)
        .expect("enum Group")
        .split('}')
        .next()
        .unwrap();
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.trim_end_matches(',').to_string())
        .collect()
}

/// State is never colour only: every session state has a glyph, all five
/// states have five different shapes, and each tone
/// class the glyph names has a rule that colours it.
#[test]
fn every_state_has_a_glyph_shape_besides_colour() {
    let table = glyph_table();
    let variants = group_variants();
    assert_eq!(variants.len(), 5, "Group changed: {variants:?}");
    for v in &variants {
        assert!(
            table.iter().any(|(g, ..)| g == v),
            "state {v} has no glyph arm"
        );
    }
    let shapes: Vec<&str> = table.iter().map(|(_, s, _)| s.as_str()).collect();
    let mut uniq = shapes.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(
        uniq.len(),
        shapes.len(),
        "two states share a shape: {table:?}"
    );
    let css = res("resources/sessions.css");
    for (g, _, tone) in &table {
        assert!(
            css.contains(&format!("glyph.{tone} {{")),
            "state {g}: no rule for glyph.{tone}"
        );
    }
}

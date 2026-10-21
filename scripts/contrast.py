#!/usr/bin/env python3
"""Contraste WCAG 2.x dos tokens da GUI, nos dois temas.

    python3 -I scripts/contrast.py [crates/agentcenter/resources/tokens.css]

Le o `:root` de topo (tema claro) e o `:root` dentro de
`@media (prefers-color-scheme: dark)` (escuro, sobre o claro). Resolve
`var(--x)` e cores `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb()`/`rgba()`.

O que se mede vem do nome do token, para nenhum par ficar de fora:

- `--ac-surface-*`: superficies onde ha texto ou marca.
- `--ac-text-*`: texto de corpo; >= 4.5:1 sobre TODA superficie.
- `--ac-text-large-*`: texto grande (>= 14pt negrito ou 18pt); >= 3:1.
- `--ac-mark-*`: marca nao textual (foco, fatia de gauge); >= 3:1 sobre toda
  superficie e sobre `--ac-track` (WCAG 1.4.11).
- `--ac-on-<fundo>-*`: texto sobre um fundo colorido `--ac-<fundo>` (pilula,
  selecao); >= 4.5:1 so contra ele.
- `--ac-text-rec-*` e `--ac-mark-rec-*` (estados do cockpit, C24): alem das
  superficies, tambem sobre `--ac-selection`, onde a pilula fica quando a
  linha esta selecionada. `--ac-mark-focus` tambem sobre `--ac-selection`:
  voltando do detalhe o anel cai na linha selecionada (C11). Os estados de `STATES` sao obrigatorios: faltando
  um num tema, o script sai 2.
- `--dim-opacity` (libadwaita): `.dim-label` e `--ac-text-primary` com essa
  opacidade; >= 4.5:1 sobre toda superficie, composto sobre ela.

Sai 1 se algum par falhar, 2 se o arquivo nao tiver o que medir.
"""

import re
import sys
from pathlib import Path

DEFAULT = Path(__file__).resolve().parent.parent / "crates/agentcenter/resources/tokens.css"
TEXT_MIN, LARGE_MIN, MARK_MIN = 4.5, 3.0, 3.0
# Estados do cockpit que a GUI pinta (ok, compactar, handoff): texto e marca
# de cada um, e o fundo com o texto por cima do que e preenchido.
STATES = (
    "--ac-text-rec-ok", "--ac-mark-rec-ok",
    "--ac-text-rec-compact", "--ac-mark-rec-compact",
    "--ac-rec-handoff", "--ac-on-rec-handoff-text",
)


def blocks(css):
    css = re.sub(r"/\*.*?\*/", "", css, flags=re.S)
    light, dark = {}, {}
    media = re.search(r"@media\s*\(\s*prefers-color-scheme\s*:\s*dark\s*\)\s*\{", css)
    top, inner = css, ""
    if media:
        # Fecha o @media contando chaves.
        depth, i = 1, media.end()
        while depth and i < len(css):
            depth += {"{": 1, "}": -1}.get(css[i], 0)
            i += 1
        inner = css[media.end():i - 1]
        top = css[:media.start()] + css[i:]
    for src, out in ((top, light), (inner, dark)):
        for body in re.findall(r":root\s*\{(.*?)\}", src, flags=re.S):
            for name, value in re.findall(r"(--[\w-]+)\s*:\s*([^;]+);", body):
                out[name] = value.strip()
    return light, dark


def parse_color(v):
    v = v.strip()
    m = re.fullmatch(r"#([0-9a-fA-F]{3,8})", v)
    if m:
        h = m.group(1)
        if len(h) in (3, 4):
            h = "".join(c * 2 for c in h)
        if len(h) not in (6, 8):
            return None
        r, g, b = (int(h[i:i + 2], 16) / 255 for i in (0, 2, 4))
        a = int(h[6:8], 16) / 255 if len(h) == 8 else 1.0
        return (r, g, b, a)
    m = re.fullmatch(r"rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)(?:\s*[,/]\s*([\d.]+%?))?\s*\)", v)
    if m:
        r, g, b = (float(m.group(i)) / 255 for i in (1, 2, 3))
        a = m.group(4)
        alpha = 1.0 if a is None else (float(a[:-1]) / 100 if a.endswith("%") else float(a))
        return (r, g, b, alpha)
    return None


def resolve(name, env, seen=()):
    if name in seen or name not in env:
        return None
    v = env[name]
    m = re.fullmatch(r"var\(\s*(--[\w-]+)\s*\)", v)
    if m:
        return resolve(m.group(1), env, seen + (name,))
    return v


def lum(c):
    def ch(x):
        return x / 12.92 if x <= 0.04045 else ((x + 0.055) / 1.055) ** 2.4
    r, g, b = (ch(x) for x in c[:3])
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def over(fg, bg):
    a = fg[3]
    return tuple(fg[i] * a + bg[i] * (1 - a) for i in range(3)) + (1.0,)


def ratio(fg, bg):
    fg = over(fg, bg)
    a, b = sorted((lum(fg), lum(bg)), reverse=True)
    return (a + 0.05) / (b + 0.05)


def theme_pairs(env):
    colors = {}
    for n in env:
        v = resolve(n, env)
        c = parse_color(v) if v else None
        if c:
            colors[n] = c
    surfaces = sorted(n for n in colors if n.startswith("--ac-surface-"))
    pairs = []
    for n in sorted(colors):
        # A pilula do cockpit tambem fica sobre a linha selecionada, e o anel
        # de foco tambem: voltando do detalhe a linha focada e a selecionada.
        on_sel = ("--ac-text-rec-", "--ac-mark-rec-", "--ac-mark-focus")
        sel = ["--ac-selection"] if n.startswith(on_sel) else []
        if n.startswith("--ac-text-large-"):
            pairs += [(n, s, LARGE_MIN) for s in surfaces]
        elif n.startswith("--ac-text-"):
            pairs += [(n, s, TEXT_MIN) for s in surfaces + sel]
        elif n.startswith("--ac-mark-"):
            pairs += [(n, s, MARK_MIN) for s in surfaces + ["--ac-track"] + sel]
        elif n.startswith("--ac-on-"):
            base = "--ac-" + n[len("--ac-on-"):].rsplit("-", 1)[0]
            pairs.append((n, base, TEXT_MIN))
    dim = env.get("--dim-opacity")
    if dim and "--ac-text-primary" in colors:
        a = float(dim.rstrip("%")) / (100 if dim.endswith("%") else 1)
        r, g, b, _ = colors["--ac-text-primary"]
        colors["--ac-text-primary@dim"] = (r, g, b, a)
        pairs += [("--ac-text-primary@dim", s, TEXT_MIN) for s in surfaces]
    return colors, surfaces, pairs


def main():
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT
    light, dark = blocks(path.read_text(encoding="utf-8"))
    themes = [("light", light), ("dark", {**light, **dark})]
    failed = checked = 0
    for theme, env in themes:
        colors, surfaces, pairs = theme_pairs(env)
        missing = [n for n in STATES if n not in colors]
        if missing:
            print(f"contrast: {path}: cockpit state tokens missing in {theme}: {', '.join(missing)}", file=sys.stderr)
            return 2
        if not surfaces or not pairs:
            print(f"contrast: {path}: no --ac-surface-* or nothing to measure in {theme}", file=sys.stderr)
            return 2
        print(f"== {theme}")
        for fg, bg, need in pairs:
            if fg not in colors or bg not in colors:
                print(f"FAIL {fg} on {bg}: unresolved")
                failed += 1
                continue
            r = ratio(colors[fg], colors[bg])
            ok = r >= need
            checked += 1
            failed += not ok
            print(f"{'ok  ' if ok else 'FAIL'} {r:5.2f}:1 (>= {need}) {fg} on {bg}")
    print(f"contrast: {checked} pairs, {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

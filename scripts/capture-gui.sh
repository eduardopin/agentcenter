#!/usr/bin/env bash
# Capturas da GUI em modo fixture: cada snapshot sintetico em 3 larguras e 2
# temas, sem ler a maquina (AGENTCENTER_SNAPSHOT_JSON).
#
#   scripts/capture-gui.sh [--detail|--reclaim] [fixture ...]   # padrao: tipico-40 nomes-longos vazio uma
#   OUT=target/screenshots BIN=target/release/agentcenter
#   SIZES="1280x800 840x700 360x640" THEMES="light dark"
#
# Saida: $OUT/<fixture>-<LxA>-<tema>.png (4 x 3 x 2 = 24). O tema e forcado
# por ADW_DEBUG_COLOR_SCHEME=prefer-light|prefer-dark. Falha (exit 1) se uma
# captura nao sair, se o PNG nao tiver o tamanho pedido, se o GTK reclamar
# do CSS ou se a app acusar `agentcenter: overflow:` (rolagem lateral com
# hadjustment.upper > page_size, widget fora da janela, rotulo cortado; ver
# measure.rs). Cada linha ok traz band_px, a altura da faixa da maquina
# (e reclaim_pids com --reclaim).
#
# --detail: abre na partida o detalhe da sessao com mais processos
# (AGENTCENTER_DETAIL=busiest, C11): lado a lado com a lista em 1280 e 840,
# empurrado por cima dela em 360. Saida: $OUT/<fixture>-detail-<LxA>-<tema>.png.
# DETAIL_SESSION=<pid> abre outra sessao (padrao busiest). DETAIL_BACK=1
# volta do detalhe logo depois (navigation.pop, a acao de Esc), com o foco
# visivel: a captura mostra a lista e falha se o foco nao voltar a uma linha.
# Janela inativa (tela apagada, sem foco do compositor): a app forca a janela
# ativa e o :focus-visible da linha e a linha ok diz forced=active,focus_visible.
# Fixture sem sessao (vazio) nao tem detalhe e fica de fora do padrao.
#
# --reclaim: abre na pagina Reclaim (AGENTCENTER_PAGE=reclaim, C12). A janela
# do reclaim pede 10 ticks com taxas, entao a captura espera 12 frames de
# dados (measure.rs); com o tick de 200 ms sai em ~2,5 s. Padrao: tipico-40
# (ociosas por status e por CPU), nomes-longos e vazio (estado sem ociosas).
# Saida: $OUT/<fixture>-reclaim-<LxA>-<tema>.png. Nao combina com --detail.
# Precisa de sessao grafica (WAYLAND_DISPLAY ou DISPLAY).
set -euo pipefail
cd "$(dirname "$0")/.."

OUT=${OUT:-target/screenshots}
BIN=${BIN:-target/release/agentcenter}
SIZES=${SIZES:-"1280x800 840x700 360x640"}
THEMES=${THEMES:-"light dark"}
FIXTURES_DIR=crates/agentcenter/tests/fixtures/snapshots
DETAIL=
PAGE=
args=()
for arg in "$@"; do
    case $arg in
        --detail) DETAIL=${DETAIL_SESSION:-busiest} ;;
        --reclaim) PAGE=reclaim ;;
        -*) echo "capture-gui: unknown option $arg (only --detail, --reclaim)" >&2; exit 2 ;;
        *) args+=("$arg") ;;
    esac
done
if [[ -n $DETAIL && -n $PAGE ]]; then
    echo "capture-gui: --detail and --reclaim open different pages; pick one" >&2
    exit 2
fi
if ((${#args[@]})); then
    fixtures=("${args[@]}")
elif [[ -n $PAGE ]]; then
    fixtures=(tipico-40 nomes-longos vazio)
elif [[ -n $DETAIL ]]; then
    fixtures=(tipico-40 nomes-longos uma)
else
    fixtures=(tipico-40 nomes-longos vazio uma)
fi

if [[ -z ${WAYLAND_DISPLAY:-} && -z ${DISPLAY:-} ]]; then
    echo "capture-gui: no WAYLAND_DISPLAY or DISPLAY: needs a graphical session" >&2
    exit 2
fi
if [[ ! -x $BIN ]]; then
    cargo build --release -p agentcenter >&2
fi
mkdir -p "$OUT"

png_size() { python3 -I -c 'import struct, sys; b = open(sys.argv[1], "rb").read(24); print("%dx%d" % struct.unpack(">II", b[16:24]))' "$1"; }

# A volta (DETAIL_BACK) espera a transicao, que com a tela apagada passa de
# 0,75 s: 600 ms por tick poem a captura (3 frames) depois dela. INTERVAL_MS
# troca o tick.
interval=${INTERVAL_MS:-200}
[[ ${DETAIL_BACK:-} == 1 ]] && interval=${INTERVAL_MS:-600}
n=0 failed=0
for fx in "${fixtures[@]}"; do
    json=$FIXTURES_DIR/$fx.json
    [[ -f $json ]] || { echo "capture-gui: no fixture $json" >&2; exit 2; }
    for size in $SIZES; do
        for theme in $THEMES; do
            png=$OUT/$fx${DETAIL:+-detail}${PAGE:+-$PAGE}-$size-$theme.png
            rm -f "$png"
            # 200 ms por tick: os 3 frames de dados da captura saem em ~0,5 s.
            err=$(AGENTCENTER_SNAPSHOT_JSON=$json AGENTCENTER_SNAPSHOT_INTERVAL_MS=$interval \
                ADW_DEBUG_COLOR_SCHEME=prefer-$theme AGENTCENTER_NON_UNIQUE=1 \
                AGENTCENTER_DETAIL=$DETAIL AGENTCENTER_DETAIL_BACK=${DETAIL_BACK:-} \
                AGENTCENTER_PAGE=$PAGE \
                AGENTCENTER_SCREENSHOT=$png AGENTCENTER_SCREENSHOT_SIZE=$size \
                timeout 30 "$BIN" 2>&1 >"$OUT/.stdout") || true
            got=$([[ -s $png ]] && png_size "$png" || echo none)
            band=$(sed -n 's/^band_px=//p' "$OUT/.stdout")
            back=$(sed -n 's/^back_focus=//p' "$OUT/.stdout")
            forced=$(sed -n 's/^back_forced=//p' "$OUT/.stdout")
            pids=$(sed -n 's/^reclaim_pids=\([^ ]*\).*/\1/p' "$OUT/.stdout")
            # Sem reclaim_pids= a janela nao encheu: a captura seria o "watching".
            if [[ -n $PAGE ]] && ! grep -q '^reclaim_pids=' "$OUT/.stdout"; then
                err+="${err:+; }agentcenter: overflow: the reclaim window never filled (no reclaim_pids=)"
            fi
            if [[ -n $DETAIL && ${DETAIL_BACK:-} == 1 && $back != GtkListItemWidget\ in=GtkListView* ]]; then
                err+="${err:+; }agentcenter: overflow: back from the detail left the focus on ${back:-nothing}"
            fi
            if [[ $got != "$size" ]] || grep -qiE 'css|theme parser|agentcenter: (overflow|AGENTCENTER_DETAIL)' <<<"$err"; then
                echo "FAIL $png: size $got${err:+, stderr: $err}" >&2
                failed=$((failed + 1))
            else
                echo "ok   $png band_px=${band:-?}${PAGE:+ reclaim_pids=${pids:-none}}${back:+ back_focus=$back}${forced:+ forced=$forced}"
                n=$((n + 1))
            fi
        done
    done
done
rm -f "$OUT/.stdout"
echo "capture-gui: $n PNG in $OUT, $failed failed"
((failed == 0))

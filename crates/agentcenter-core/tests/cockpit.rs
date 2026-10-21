//! Cockpit (C20): as contas, a recomendacao e os limiares contra o
//! `contexto.py` do squad, o provider e a idade do handoff em diretorio
//! temporario.
//!
//! Os valores de referencia foram gerados pelo proprio script, com o ambiente
//! sem `SQUAD_*` nem `CLAUDE_SESSION_BUDGET` (as variaveis de cada caso de
//! ambiente sao postas so nele), um comando por caso:
//!
//! ```sh
//! env -u SQUAD_CTX_PCT -u SQUAD_REREAD -u SQUAD_REREAD_PISO -u CLAUDE_SESSION_BUDGET \
//!     -u SQUAD_BURN_CHEIO -u SQUAD_CTX_BASE [VAR=valor] python3 -I -c \
//!   "import sys; sys.path.insert(0,'/path/to/agentic-swarm/skills/contexto-de-sessao/scripts'); \
//!    import contexto as cx; print(cx.HANDOFF_NOW, cx.REREAD_SHARE, cx.REREAD_FLOOR_PCT, cx.BUDGET, \
//!    cx.BURN_FULL_PCT, cx.CTX_BASE, cx.contas(1000,200000,3000000,20000,800000,1000000))"
//! ```
//!
//! (os casos A-G trocam o `print` por `cx.contas(<argumentos do caso>)`).

use agentcenter_core::cockpit::{
    self, Cockpit, Contas, Family, HandoffIndex, Loops, Reason, Recommendation, Thresholds, Tokens,
    cache_hit, ctx_pct, family, latest_handoff, provider, round_n,
};
use agentcenter_core::fixture::TempDir;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[allow(clippy::too_many_arguments)]
fn contas(
    novo: u64,
    relei: u64,
    burn: u64,
    burn_pct: f64,
    razao: f64,
    share: f64,
    piso: u64,
    piso_min: u64,
    turno: u64,
    payback: Option<f64>,
    trocar: bool,
) -> Contas {
    Contas {
        novo,
        relei,
        burn,
        burn_pct,
        razao,
        share,
        piso,
        piso_min,
        turno,
        reinicio: 60000,
        payback,
        trocar,
    }
}

fn th() -> Thresholds {
    Thresholds::from_env(|_| None)
}

fn th_with(vars: &[(&str, &str)]) -> Thresholds {
    let map: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    Thresholds::from_env(move |k| map.get(k).cloned())
}

/// Veredito so pelas contas e pelo contexto, sem ociosidade, loop nem handoff.
fn verdict(t: &Thresholds, c: &Contas, ctx: u64, window: u64) -> Recommendation {
    t.recommend(ctx_pct(ctx, window), c, None, None, None).0
}

// --- 1) tabela de referencia --------------------------------------------------

#[test]
fn table_a_matches_script() {
    let t = th();
    let c = t.contas(16, 51200, 154455, 2670, 122456, 1_000_000);
    let want = contas(
        77366,
        15445,
        92811,
        0.5,
        0.2,
        0.166,
        898143,
        700000,
        12245,
        Some(7.3),
        false,
    );
    assert_eq!(c, want);
    assert_eq!(cache_hit(16, 51200, 154455), Some(0.751));
    assert_eq!(
        verdict(&t, &c, 122456, 1_000_000),
        Recommendation::Continuar
    );
}

#[test]
fn table_b_matches_script_and_compacts() {
    let t = th();
    let c = t.contas(1000, 200000, 3000000, 20000, 550000, 1_000_000);
    let want = contas(
        351000,
        300000,
        651000,
        3.3,
        0.855,
        0.461,
        886980,
        700000,
        55000,
        Some(1.2),
        false,
    );
    assert_eq!(c, want);
    assert_eq!(
        verdict(&t, &c, 550000, 1_000_000),
        Recommendation::Compactar
    );
}

#[test]
fn table_c_matches_script_and_hands_off() {
    let t = th();
    let c = t.contas(1000, 300000, 5000000, 30000, 900000, 1_000_000);
    let want = contas(
        526000,
        500000,
        1026000,
        5.1,
        0.951,
        0.487,
        879480,
        700000,
        90000,
        Some(0.7),
        false,
    );
    assert_eq!(c, want);
    assert_eq!(verdict(&t, &c, 900000, 1_000_000), Recommendation::Handoff);
}

#[test]
fn table_d_matches_script_and_swaps() {
    let t = th();
    let c = t.contas(5000, 600000, 80000000, 100000, 720000, 1_000_000);
    let want = contas(
        1255000,
        8000000,
        9255000,
        46.3,
        6.375,
        0.864,
        714900,
        700000,
        72000,
        Some(0.9),
        true,
    );
    assert_eq!(c, want);
    let (v, why) = t.recommend(ctx_pct(720000, 1_000_000), &c, None, None, None);
    assert_eq!(v, Recommendation::Handoff);
    assert_eq!(
        why,
        vec![
            Reason::ContextoAlto {
                ctx_pct: 72.0,
                limite_pct: 50
            },
            Reason::Releitura {
                share: 0.864,
                limite: 0.6,
                piso: 714900,
                turno: 72000,
                payback: Some(0.9),
            },
        ]
    );
}

#[test]
fn table_e_high_reread_with_low_context_continues() {
    let t = th();
    let c = t.contas(100, 50000, 9000000, 5000, 120000, 1_000_000);
    let want = contas(
        87600,
        900000,
        987600,
        4.9,
        10.274,
        0.911,
        880248,
        700000,
        12000,
        Some(7.5),
        false,
    );
    assert_eq!(c, want);
    assert_eq!(
        verdict(&t, &c, 120000, 1_000_000),
        Recommendation::Continuar
    );
}

#[test]
fn table_f_unknown_window_floors_at_twice_ctx_base() {
    let t = th();
    let c = t.contas(100, 50000, 9000000, 5000, 120000, 0);
    let want = contas(
        87600,
        900000,
        987600,
        4.9,
        10.274,
        0.911,
        80000,
        80000,
        12000,
        Some(7.5),
        true,
    );
    assert_eq!(c, want);
    assert_eq!(ctx_pct(120000, 0), 0.0);
    assert_eq!(verdict(&t, &c, 120000, 0), Recommendation::Handoff);
}

#[test]
fn table_g_empty_session() {
    let t = th();
    let c = t.contas(0, 0, 0, 0, 0, 128000);
    let want = contas(0, 0, 0, 0.0, 0.0, 0.0, 115200, 89600, 0, None, false);
    assert_eq!(c, want);
    assert_eq!(cache_hit(0, 0, 0), None);
}

#[test]
fn swap_uses_the_unrounded_share() {
    // contas(4004,0,59960,0,1000000,0): share exibido 0.6, real 0.5996 -> trocar False;
    // contas(4000,0,60000,0,1000000,0): share 0.6 -> trocar True.
    let t = th();
    let c = t.contas(4004, 0, 59960, 0, 1_000_000, 0);
    assert_eq!(
        (c.share, c.razao, c.piso, c.payback),
        (0.6, 1.498, 80000, Some(0.6))
    );
    assert!(!c.trocar);
    let c = t.contas(4000, 0, 60000, 0, 1_000_000, 0);
    assert_eq!((c.share, c.razao), (0.6, 1.5));
    assert!(c.trocar);
}

#[test]
fn cache_hit_is_raw_tokens_not_the_weighted_share() {
    // Mesma sessao do caso A: 75 % de cache hit, 16,6 % de releitura ponderada.
    let c = th().contas(16, 51200, 154455, 2670, 122456, 1_000_000);
    assert_eq!(cache_hit(16, 51200, 154455), Some(0.751));
    assert_eq!(c.share, 0.166);
}

#[test]
fn round_n_is_python_round() {
    // python3 -I -c "print(round(12.25,1), round(0.35,1), round(2.675,2), round(0.0005,3),
    //                      round(0.1665,3), round(7.25,1), round(1e-9,3))"
    // -> 12.2 0.3 2.67 0.001 0.167 7.2 0.0
    assert_eq!(round_n(12.25, 1), 12.2);
    assert_eq!(round_n(0.35, 1), 0.3);
    assert_eq!(round_n(2.675, 2), 2.67);
    assert_eq!(round_n(0.0005, 3), 0.001);
    assert_eq!(round_n(0.1665, 3), 0.167);
    assert_eq!(round_n(7.25, 1), 7.2);
    assert_eq!(round_n(1e-9, 3), 0.0);
}

// --- 2) precedencia -----------------------------------------------------------

/// Contas sem releitura, so com contexto: `trocar` falso.
fn plain(t: &Thresholds, ctx: u64) -> Contas {
    let c = t.contas(0, 0, 0, 0, ctx, 1_000_000);
    assert!(!c.trocar);
    c
}

#[test]
fn just_under_handoff_now_compacts() {
    let t = th();
    let pct = ctx_pct(899960, 1_000_000);
    assert_eq!(round_n(pct, 1), 90.0); // o script exibe 90.0
    let (v, why) = t.recommend(pct, &plain(&t, 899960), None, None, None);
    assert_eq!(v, Recommendation::Compactar);
    assert_eq!(
        why,
        vec![Reason::ContextoAlto {
            ctx_pct: pct,
            limite_pct: 50
        }]
    );
}

#[test]
fn idle_beats_compact() {
    let t = th();
    let (v, why) = t.recommend(60.0, &plain(&t, 600000), Some(91.0), None, None);
    assert_eq!(v, Recommendation::Ociosa);
    assert_eq!(why, vec![Reason::Ociosa { idle_min: 91.0 }]);
}

#[test]
fn idle_at_exactly_90_minutes_is_not_idle() {
    let t = th();
    let (v, _) = t.recommend(10.0, &plain(&t, 100000), Some(90.0), None, None);
    assert_eq!(v, Recommendation::Continuar);
}

#[test]
fn handoff_now_beats_idle() {
    let t = th();
    let (v, why) = t.recommend(90.0, &plain(&t, 900000), Some(91.0), None, None);
    assert_eq!(v, Recommendation::Handoff);
    assert_eq!(
        why,
        vec![
            Reason::Ociosa { idle_min: 91.0 },
            Reason::ContextoUrgente {
                ctx_pct: 90.0,
                limite_pct: 90
            },
        ]
    );
}

#[test]
fn idle_beats_swap() {
    let t = th();
    let c = t.contas(5000, 600000, 80000000, 100000, 720000, 1_000_000);
    assert!(c.trocar);
    let (v, why) = t.recommend(72.0, &c, Some(91.0), None, None);
    assert_eq!(v, Recommendation::Ociosa);
    assert_eq!(why.len(), 2);
    assert_eq!(why[0], Reason::Ociosa { idle_min: 91.0 });
    assert!(matches!(why[1], Reason::Releitura { .. }));
}

#[test]
fn old_handoff_only_adds_a_reason() {
    let t = th();
    let (v, why) = t.recommend(55.0, &plain(&t, 550000), None, Some(300.0), None);
    assert_eq!(v, Recommendation::Compactar);
    assert_eq!(
        why,
        vec![
            Reason::ContextoAlto {
                ctx_pct: 55.0,
                limite_pct: 50
            },
            Reason::HandoffVelho { age_min: 300.0 },
        ]
    );

    let (v, why) = t.recommend(40.0, &plain(&t, 400000), None, Some(300.0), None);
    assert_eq!(v, Recommendation::Continuar);
    assert!(why.is_empty());

    // 240 min em ponto: ainda nao e velho (o script usa `>`).
    let (_, why) = t.recommend(55.0, &plain(&t, 550000), None, Some(240.0), None);
    assert!(!why.contains(&Reason::HandoffVelho { age_min: 240.0 }));
}

#[test]
fn loop_signal_beats_idle_but_not_handoff_now() {
    let t = th();
    let (v, why) = t.recommend(60.0, &plain(&t, 600000), Some(91.0), None, Some(3));
    assert_eq!(v, Recommendation::Loop);
    assert_eq!(
        why,
        vec![
            Reason::Ociosa { idle_min: 91.0 },
            Reason::Loop { repeats: 3 }
        ]
    );
    let (v, _) = t.recommend(95.0, &plain(&t, 950000), None, None, Some(3));
    assert_eq!(v, Recommendation::Handoff);
}

/// `rep or streak >= LOOP_REPEATS` e o `max([n for _, n in rep] + [streak])`
/// do motivo, dos dois numeros de [`Loops`].
#[test]
fn loops_repeats_follow_the_script_threshold() {
    let l = |max_streak, repeated_max| Loops {
        max_streak,
        repeated_max,
    };
    assert_eq!(cockpit::LOOP_REPEATS, 3);
    assert_eq!(l(1, 0).repeats(), None, "sem chamadas");
    assert_eq!(l(0, 0).repeats(), None, "JSON antigo, sem o sinal");
    assert_eq!(l(2, 2).repeats(), None);
    assert_eq!(l(1, 3).repeats(), Some(3), "3 iguais nao seguidas");
    assert_eq!(l(3, 3).repeats(), Some(3), "3 seguidas");
    assert_eq!(l(2, 5).repeats(), Some(5), "o motivo leva a maior contagem");
    assert_eq!(l(4, 4).repeats(), Some(4));
}

fn tokens(ctx: u64) -> Tokens {
    Tokens {
        input: 1_000,
        cache_write: 20_000,
        cache_read: 0,
        output: 500,
        ctx,
        window: 1_000_000,
        window_inferred: false,
    }
}

const LOOPING: Loops = Loops {
    max_streak: 2,
    repeated_max: 4,
};

/// No `Cockpit` inteiro: LOOP vale sobre OCIOSA (que fica como motivo antes)
/// e sobre o contexto alto (COMPACTAR so com CONTINUAR), e cede ao contexto
/// urgente; os dois numeros saem como vieram.
#[test]
fn cockpit_loop_comes_after_idle_and_before_context() {
    let t = th();
    let c = Cockpit::compute_with_loops(&t, &tokens(100_000), LOOPING, Some(91.0), None);
    assert_eq!(c.recommendation, Recommendation::Loop);
    assert_eq!(
        c.reasons,
        vec![
            Reason::Ociosa { idle_min: 91.0 },
            Reason::Loop { repeats: 4 }
        ]
    );
    assert_eq!((c.max_streak, c.repeated_max), (2, 4));

    let c = Cockpit::compute_with_loops(&t, &tokens(600_000), LOOPING, Some(1.0), None);
    assert_eq!(c.recommendation, Recommendation::Loop, "nao COMPACTAR");
    assert_eq!(c.reasons, vec![Reason::Loop { repeats: 4 }]);

    let c = Cockpit::compute_with_loops(&t, &tokens(950_000), LOOPING, Some(91.0), None);
    assert_eq!(c.recommendation, Recommendation::Handoff);
    assert_eq!(c.reasons.len(), 3);
    assert_eq!(c.reasons[1], Reason::Loop { repeats: 4 });
    assert!(matches!(c.reasons[2], Reason::ContextoUrgente { .. }));

    // Abaixo do limiar o sinal nao muda nada, so sai no JSON.
    let quiet = Loops {
        max_streak: 2,
        repeated_max: 2,
    };
    let c = Cockpit::compute_with_loops(&t, &tokens(600_000), quiet, None, None);
    assert_eq!(c.recommendation, Recommendation::Compactar);
    assert_eq!((c.max_streak, c.repeated_max), (2, 2));
}

/// `max_streak` sem nenhuma chamada e 1, como o `best` do script; 0 quer
/// dizer "sem sinal": JSON de antes do C23 e `Cockpit::compute`, que nao
/// recebe o transcript.
#[test]
fn max_streak_is_one_without_calls_and_zero_without_the_signal() {
    let t = th();
    let none = Loops {
        max_streak: 1,
        repeated_max: 0,
    };
    let c = Cockpit::compute_with_loops(&t, &tokens(100_000), none, None, None);
    assert_eq!((c.max_streak, c.repeated_max), (1, 0));
    let c = Cockpit::compute(&t, &tokens(100_000), None, None);
    assert_eq!((c.max_streak, c.repeated_max), (0, 0));
    let mut json = serde_json::to_value(&c).unwrap();
    let obj = json.as_object_mut().unwrap();
    assert!(obj.remove("max_streak").is_some() && obj.remove("repeated_max").is_some());
    let old: Cockpit = serde_json::from_value(json).unwrap();
    assert_eq!(old, c, "JSON sem os campos le 0");
}

// --- 3) ambiente --------------------------------------------------------------

#[test]
fn defaults_are_the_scripts() {
    let t = th();
    assert_eq!(t, Thresholds::default());
    assert_eq!(
        (
            t.handoff_now,
            t.reread_share,
            t.reread_floor_pct,
            t.budget,
            t.burn_full_pct,
            t.ctx_base
        ),
        (90, 0.6, 70, 20_000_000, 50, 40_000)
    );
}

/// Contas do comando do topo (`contas(1000,200000,3000000,20000,800000,1M)`).
fn env_case(t: &Thresholds) -> Contas {
    t.contas(1000, 200000, 3000000, 20000, 800000, 1_000_000)
}

#[test]
fn ctx_pct_80_hands_off_at_800k() {
    // script: 80 0.6 70 20000000 50 40000 {... 'piso': 793490, 'piso_min': 700000, 'turno': 80000,
    //         'reinicio': 60000, 'payback': 0.8, 'trocar': False}
    let t = th_with(&[("SQUAD_CTX_PCT", "80")]);
    assert_eq!(t.handoff_now, 80);
    let c = env_case(&t);
    assert_eq!(
        (c.piso, c.piso_min, c.payback, c.trocar),
        (793490, 700000, Some(0.8), false)
    );
    assert_eq!(verdict(&t, &c, 800000, 1_000_000), Recommendation::Handoff);
    // Sem a variavel o mesmo contexto so compacta.
    assert_eq!(
        verdict(&th(), &env_case(&th()), 800000, 1_000_000),
        Recommendation::Compactar
    );
}

#[test]
fn invalid_or_empty_values_fall_back_to_default() {
    // script com SQUAD_CTX_PCT=abc, "" e 85.0: 90, piso 886980.
    for raw in ["abc", "", "85.0", "8__5", "_85", "85_", "- 5"] {
        let t = th_with(&[("SQUAD_CTX_PCT", raw)]);
        assert_eq!(t.handoff_now, 90, "{raw:?}");
        assert_eq!(env_case(&t).piso, 886980, "{raw:?}");
    }
}

#[test]
fn python_int_syntax_is_accepted() {
    // script com SQUAD_CTX_PCT=" 85 ", "8_5" e "+85": 85, piso 840235.
    for raw in [" 85 ", "8_5", "+85", "085"] {
        let t = th_with(&[("SQUAD_CTX_PCT", raw)]);
        assert_eq!(t.handoff_now, 85, "{raw:?}");
        assert_eq!(env_case(&t).piso, 840235, "{raw:?}");
    }
}

#[test]
fn values_are_clamped_like_the_script() {
    // SQUAD_CTX_PCT=5 -> 10; piso e piso_min 100000.
    let t = th_with(&[("SQUAD_CTX_PCT", "5")]);
    assert_eq!(t.handoff_now, 10);
    let c = env_case(&t);
    assert_eq!((c.piso, c.piso_min), (100000, 100000));

    // SQUAD_REREAD_PISO=0 -> 0; piso 841410, piso_min 80000.
    let t = th_with(&[("SQUAD_REREAD_PISO", "0")]);
    assert_eq!(t.reread_floor_pct, 0);
    let c = env_case(&t);
    assert_eq!((c.piso, c.piso_min), (841410, 80000));

    // CLAUDE_SESSION_BUDGET=1 -> 1000000; burn_pct 65.1, piso 700000.
    let t = th_with(&[("CLAUDE_SESSION_BUDGET", "1")]);
    assert_eq!(t.budget, 1_000_000);
    let c = env_case(&t);
    assert_eq!((c.burn_pct, c.piso, c.piso_min), (65.1, 700000, 700000));

    // SQUAD_REREAD=99 -> 0.95.
    assert_eq!(th_with(&[("SQUAD_REREAD", "99")]).reread_share, 0.95);

    // SQUAD_BURN_CHEIO=0 -> 1; piso 700000.
    let t = th_with(&[("SQUAD_BURN_CHEIO", "0")]);
    assert_eq!(t.burn_full_pct, 1);
    assert_eq!(env_case(&t).piso, 700000);

    // SQUAD_CTX_BASE=999999999999999999999 -> 500000; piso 1000000, reinicio 635000, payback 21.2.
    let t = th_with(&[("SQUAD_CTX_BASE", "999999999999999999999")]);
    assert_eq!(t.ctx_base, 500_000);
    let c = env_case(&t);
    assert_eq!(
        (c.piso, c.piso_min, c.reinicio, c.payback),
        (1_000_000, 1_000_000, 635000, Some(21.2))
    );
}

// --- 4) handoff ---------------------------------------------------------------

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

fn ago(min: u64) -> SystemTime {
    now() - Duration::from_secs(min * 60)
}

fn write_md(dir: &Path, name: &str, mtime: SystemTime) {
    fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    fs::write(&p, "# handoff\n").unwrap();
    set_mtime(&p, mtime);
}

fn set_mtime(p: &Path, mtime: SystemTime) {
    // So o teste abre arquivos: e o jeito de ajustar o mtime pela std.
    fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
}

fn set_dir_mtime(p: &Path, mtime: SystemTime) {
    fs::File::open(p).unwrap().set_modified(mtime).unwrap();
}

/// Diretorio temporario e a raiz dele absoluta, sem `..` e sem link: o
/// `TMPDIR` pode ser relativo ou ter `..`, e o leitor normaliza o cwd como o
/// `abspath`, entao as expectativas sao montadas sobre a raiz normalizada.
fn scratch(prefix: &str) -> (TempDir, PathBuf) {
    let tmp = TempDir::new(prefix).unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    (tmp, root)
}

/// Repo em `<tmp>/repo` com `.git` diretorio e dois handoffs, mais ruido que o
/// glob `*.md` ignora (oculto, outra extensao). A subida para no `.git` dele,
/// entao nada acima do diretorio temporario entra.
fn repo(tmp: &Path) -> PathBuf {
    let root = tmp.join("repo");
    fs::create_dir_all(root.join(".git")).unwrap();
    let ho = root.join(".claude").join("handoffs");
    write_md(&ho, "2026-10-16-velho.md", ago(60));
    write_md(&ho, "2026-10-17-novo.md", ago(10));
    write_md(&ho, ".oculto.md", ago(1));
    write_md(&ho, "notas.txt", ago(1));
    root
}

#[test]
fn repo_handoff_is_the_newest() {
    let (_tmp, tmp) = scratch("ac-cockpit-ho-a");
    let root = repo(&tmp);
    let home = tmp.join("home");
    let h = latest_handoff(&root, &home, now()).unwrap();
    assert_eq!(h.path, root.join(".claude/handoffs/2026-10-17-novo.md"));
    assert_eq!(h.age_min, 10.0);
}

#[test]
fn subdirectory_climbs_to_repo_root() {
    let (_tmp, tmp) = scratch("ac-cockpit-ho-b");
    let root = repo(&tmp);
    let deep = root.join("src").join("deep");
    fs::create_dir_all(&deep).unwrap();
    let home = tmp.join("home");
    let h = latest_handoff(&deep, &home, now()).unwrap();
    assert_eq!(h.path, root.join(".claude/handoffs/2026-10-17-novo.md"));
    // `..` e normalizado como no abspath.
    let h2 = latest_handoff(&deep.join("..").join("."), &home, now()).unwrap();
    assert_eq!(h2, h);
}

#[test]
fn worktree_stops_at_its_own_root() {
    let (_tmp, tmp) = scratch("ac-cockpit-ho-c");
    let root = repo(&tmp);
    let wt = root.join(".claude").join("worktrees").join("wt");
    fs::create_dir_all(&wt).unwrap();
    fs::write(wt.join(".git"), "gitdir: ../../../.git/worktrees/wt\n").unwrap();
    let home = tmp.join("home");
    write_md(&home.join(".claude/handoffs"), "outro-projeto.md", ago(5));
    // Como o script hoje: o repo pai tem handoffs, mas a worktree nao os ve.
    assert_eq!(latest_handoff(&wt, &home, now()), None);
    // Com handoff proprio, e ele.
    write_md(&wt.join(".claude/handoffs"), "wt.md", ago(7));
    let h = latest_handoff(&wt.join("sub"), &home, now()).unwrap();
    assert_eq!(h.path, wt.join(".claude/handoffs/wt.md"));
    assert_eq!(h.age_min, 7.0);
}

#[test]
fn outside_repo_falls_back_to_global_by_name() {
    // A raiz da fixture faz o papel do `/`: sem isso a subida passaria pelos
    // ancestrais do `TMPDIR`, que podem ser um repo com handoffs. Para provar
    // que a raiz nao e olhada, ela tem `.git` e um handoff que nao pode ganhar.
    let (_tmp, top) = scratch("ac-cockpit-ho-d");
    fs::create_dir_all(top.join(".git")).unwrap();
    write_md(&top.join(".claude/handoffs"), "raiz.md", ago(0));
    let latest = |cwd: &Path, home: &Path| HandoffIndex::new().latest_below(cwd, home, now(), &top);
    let tmp = top.join("chao");
    let home = tmp.join("fakehome");
    let global = home.join(".claude").join("handoffs");
    write_md(&global, "2026-10-17-my-proj-handoff.md", ago(20));
    write_md(&global, "2026-10-17-outra-coisa.md", ago(1));
    write_md(&global, "2026-10-17-ab.md", ago(1)); // "ab" tem so 2 letras

    let cwd = tmp.join("home").join("u").join("My_Proj");
    fs::create_dir_all(&cwd).unwrap();
    let h = latest(&cwd, &home).unwrap();
    assert_eq!(h.path, global.join("2026-10-17-my-proj-handoff.md"));
    assert_eq!(h.age_min, 20.0);

    // Herdado do script, nao corrigido: o diretorio "home" do caminho casa com
    // um handoff global que tem "home" no nome, de qualquer projeto.
    write_md(&global, "home-office.md", ago(3));
    let other = tmp.join("home").join("u").join("ab");
    fs::create_dir_all(&other).unwrap();
    let h = latest(&other, &home).unwrap();
    assert_eq!(h.path, global.join("home-office.md"));
    assert_eq!(h.age_min, 3.0);

    // Nenhum nome casa: None.
    let none = tmp.join("zz");
    fs::create_dir_all(&none).unwrap();
    let empty_home = tmp.join("vazio");
    assert_eq!(latest(&none, &empty_home), None);
    assert_eq!(latest(Path::new(""), &home), None);
}

#[test]
fn unreadable_handoff_still_has_an_age() {
    let (_tmp, tmp) = scratch("ac-cockpit-ho-e");
    let root = tmp.join("repo");
    fs::create_dir_all(root.join(".git")).unwrap();
    let ho = root.join(".claude").join("handoffs");
    write_md(&ho, "segredo.md", ago(15));
    fs::write(ho.join("segredo.md"), "CANARIO").unwrap();
    set_mtime(&ho.join("segredo.md"), ago(15));
    fs::set_permissions(ho.join("segredo.md"), fs::Permissions::from_mode(0o000)).unwrap();

    let h = latest_handoff(&root, &tmp.join("home"), now()).unwrap();
    assert_eq!(h.path, ho.join("segredo.md"));
    assert_eq!(h.age_min, 15.0);
    fs::set_permissions(ho.join("segredo.md"), fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn handoff_reader_never_opens_a_file() {
    // Com permissao 000 um open falharia (fora do root); aqui a garantia e
    // estatica: o leitor nao tem nenhuma chamada que abra ou leia arquivo.
    let src = include_str!("../src/cockpit/handoff.rs");
    for call in [
        "File::open",
        "File::options",
        "OpenOptions",
        "read_to_string",
        "fs::read(",
        "read_link",
    ] {
        assert!(!src.contains(call), "handoff.rs usa {call}");
    }
}

#[test]
fn unchanged_directory_is_not_listed_again() {
    let (_tmp, tmp) = scratch("ac-cockpit-ho-f");
    let root = repo(&tmp);
    let ho = root.join(".claude").join("handoffs");
    let home = tmp.join("home");
    // Diretorio parado ha uma hora: a listagem pode ser reaproveitada.
    set_dir_mtime(&ho, SystemTime::now() - Duration::from_secs(3600));

    let mut idx = HandoffIndex::new();
    let first = idx.latest(&root, &home, now()).unwrap();
    assert_eq!(idx.readdir_count(), 1);
    let second = idx.latest(&root, &home, now()).unwrap();
    assert_eq!(second, first);
    assert_eq!(idx.readdir_count(), 1, "diretorio inalterado nao relista");

    // Reescrever um handoff muda o mtime dele, nao o do diretorio: o stat de
    // cada consulta ve a mudanca sem relistar.
    set_mtime(&ho.join("2026-10-16-velho.md"), ago(2));
    let h = idx.latest(&root, &home, now()).unwrap();
    assert_eq!(h.path, ho.join("2026-10-16-velho.md"));
    assert_eq!(h.age_min, 2.0);
    assert_eq!(idx.readdir_count(), 1);

    // Arquivo novo muda o mtime do diretorio: relista e o ve.
    write_md(&ho, "2026-10-17-ultimo.md", ago(1));
    let h = idx.latest(&root, &home, now()).unwrap();
    assert_eq!(h.path, ho.join("2026-10-17-ultimo.md"));
    assert_eq!(idx.readdir_count(), 2);

    // Listagem de um diretorio que acabou de mudar nao e confiavel (o mtime
    // do sistema de arquivos e grosso): a proxima consulta relista.
    idx.latest(&root, &home, now()).unwrap();
    assert_eq!(idx.readdir_count(), 3);

    // Parado de novo: uma listagem e depois so cache.
    set_dir_mtime(&ho, SystemTime::now() - Duration::from_secs(3600));
    idx.latest(&root, &home, now()).unwrap();
    idx.latest(&root, &home, now()).unwrap();
    assert_eq!(idx.readdir_count(), 4);
}

// --- 5) provider --------------------------------------------------------------

#[test]
fn declared_provider_wins_and_is_normalized() {
    for (declared, want) in [
        ("anthropic", "anthropic"),
        ("deepinfra", "deepinfra"),
        ("google", "google"),
        ("mergedev", "mergedev"),
        ("openai-codex", "openai"),
        ("OpenAI-Codex ", "openai"),
        ("openai", "openai"),
        ("openrouter", "openrouter"),
        ("qwen-token-plan-individual", "qwen-token-plan-individual"),
    ] {
        assert_eq!(
            provider("pi", Some(declared), None).as_deref(),
            Some(want),
            "{declared:?}"
        );
    }
    // Declarado vence o agente.
    assert_eq!(
        provider("claude", Some("openrouter"), Some("claude-opus-4")).as_deref(),
        Some("openrouter")
    );
}

#[test]
fn invalid_declared_provider_is_dropped() {
    for bad in [
        "open ai",
        "x/y",
        "",
        "deep\u{1b}infra",
        "provedor-ç",
        &"a".repeat(33),
    ] {
        assert_eq!(
            provider("pi", Some(bad), Some("gpt-5")).as_deref(),
            Some("openai"),
            "{bad:?}"
        );
        assert_eq!(
            provider("codex", Some(bad), None).as_deref(),
            Some("openai"),
            "{bad:?}"
        );
        assert_eq!(provider("pi", Some(bad), None), None, "{bad:?}");
    }
    assert_eq!(
        provider("pi", Some(&"a".repeat(32)), None),
        Some("a".repeat(32))
    );
}

#[test]
fn provider_by_agent() {
    for (agent, want) in [
        ("claude", "anthropic"),
        ("codex", "openai"),
        ("gemini", "google"),
        ("copilot", "github"),
        ("kiro", "aws"),
        ("cursor-agent", "cursor"),
    ] {
        assert_eq!(
            provider(agent, None, None).as_deref(),
            Some(want),
            "{agent}"
        );
    }
    assert_eq!(provider("pi", None, None), None);
}

#[test]
fn provider_by_model_prefix() {
    for (model, want) in [
        ("claude-sonnet-4-5", Some("anthropic")),
        ("anthropic/claude-opus-4", Some("anthropic")),
        ("gpt-5-codex", Some("openai")),
        ("o1-mini", Some("openai")),
        ("o3", Some("openai")),
        ("o4-mini", Some("openai")),
        ("codex-mini-latest", Some("openai")),
        ("openai/gpt-5", Some("openai")),
        ("GPT-5", Some("openai")),
        ("gemini-2.5-pro", Some("google")),
        ("google/gemini-2.5-flash", Some("google")),
        ("llama-3.3-70b", None),
        ("openrouter/anthropic/claude", None),
    ] {
        assert_eq!(
            provider("pi", None, Some(model)).as_deref(),
            want,
            "{model}"
        );
    }
}

#[test]
fn provider_family() {
    assert_eq!(family("anthropic"), Family::Anthropic);
    assert_eq!(family("openai"), Family::OpenAI);
    assert_eq!(family("google"), Family::Google);
    assert_eq!(family("deepinfra"), Family::Outro);
    assert_eq!(family("github"), Family::Outro);
    assert_eq!(cockpit::family("openrouter"), Family::Outro);
}

#[test]
fn index_keeps_at_most_dirs_cap_listings_dropping_the_least_recently_used() {
    use agentcenter_core::cockpit::handoff::DIRS_CAP;
    let (_tmp, tmp) = scratch("ac-cockpit-ho-lru");
    let home = tmp.join("home");
    let repos: Vec<PathBuf> = (0..=DIRS_CAP)
        .map(|i| {
            let root = tmp.join(format!("r{i}"));
            fs::create_dir_all(root.join(".git")).unwrap();
            let ho = root.join(".claude").join("handoffs");
            write_md(&ho, "h.md", ago(5));
            // Parado ha uma hora: a listagem guardada vale.
            set_dir_mtime(&ho, SystemTime::now() - Duration::from_secs(3600));
            root
        })
        .collect();
    let mut idx = HandoffIndex::new();
    for r in &repos[..DIRS_CAP] {
        idx.latest(r, &home, now()).unwrap();
    }
    assert_eq!(idx.cached_dirs(), DIRS_CAP);
    assert_eq!(idx.readdir_count(), DIRS_CAP as u64);
    // r0 volta a ser o mais recente; o menos usado agora e r1.
    idx.latest(&repos[0], &home, now()).unwrap();
    assert_eq!(idx.readdir_count(), DIRS_CAP as u64, "r0 do cache");
    idx.latest(&repos[DIRS_CAP], &home, now()).unwrap();
    assert_eq!(idx.cached_dirs(), DIRS_CAP, "teto");
    assert_eq!(idx.readdir_count(), DIRS_CAP as u64 + 1);
    idx.latest(&repos[0], &home, now()).unwrap();
    assert_eq!(idx.readdir_count(), DIRS_CAP as u64 + 1, "r0 ficou");
    let h = idx.latest(&repos[1], &home, now()).unwrap();
    assert_eq!(h.path, repos[1].join(".claude/handoffs/h.md"));
    assert_eq!(
        idx.readdir_count(),
        DIRS_CAP as u64 + 2,
        "r1 saiu e relista"
    );
    assert_eq!(idx.cached_dirs(), DIRS_CAP);
}

#[test]
fn find_without_home_uses_only_the_local_handoff() {
    let (_tmp, tmp) = scratch("ac-cockpit-ho-nohome");
    let root = repo(&tmp);
    let mut idx = HandoffIndex::new();
    let h = idx.find(&root, None, now()).unwrap();
    assert_eq!(h.path, root.join(".claude/handoffs/2026-10-17-novo.md"));
    // Sem local, o global por nome so com home.
    let bare = tmp.join("projeto-sem-local");
    fs::create_dir_all(bare.join(".git")).unwrap();
    let home = tmp.join("home");
    write_md(
        &home.join(".claude").join("handoffs"),
        "projeto-sem-local-1.md",
        ago(3),
    );
    assert_eq!(idx.find(&bare, None, now()), None);
    let h = idx.find(&bare, Some(&home), now()).unwrap();
    assert_eq!(h.age_min, 3.0);
}

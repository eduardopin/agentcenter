//! Tabela de adapters sobre `tests/fixtures/adapters/<agente>/<caso>/`: cada caso
//! tem `comm` e `cmdline` como no /proc (cmdline com NUL entre argumentos) e
//! `exe` com o alvo do link `/proc/<pid>/exe` em texto. `nenhum/` sao processos
//! que nao podem virar sessao.

use agentcenter_core::adapters::{
    HostedCmd, Interpreter, Registry, Target, interpreter, parse_extra_agents,
};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// (caso, agente esperado). Todo diretorio de fixture tem de estar aqui.
const TABLE: &[(&str, Option<&str>)] = &[
    ("gemini/npm-bin", Some("gemini")),
    ("gemini/relancado", Some("gemini")),
    ("gemini/dist-index", Some("gemini")),
    ("copilot/nativo", Some("copilot")),
    ("copilot/npm-loader", None),
    ("kiro/chat", Some("kiro")),
    ("kiro/lancador", None),
    ("opencode/npm", Some("opencode")),
    ("opencode/caminho-exe", Some("opencode")),
    ("aider/shebang-venv", Some("aider")),
    ("aider/env-python3", Some("aider")),
    ("aider/modulo", Some("aider")),
    ("aider/modulo-main", Some("aider")),
    ("goose/sessao", Some("goose")),
    ("cursor_agent/cursor-agent", Some("cursor-agent")),
    ("cursor_agent/agent", Some("cursor-agent")),
    ("cursor_agent/worker", None),
    ("cursor_agent/other_index", None),
    ("nenhum/node-context7-mcp", None),
    ("nenhum/node-pnpm-run-test", None),
    ("nenhum/node-require-tsx", None),
    ("nenhum/node-menciona-gemini", None),
    ("nenhum/python-pytest", None),
    ("nenhum/python-c-aider", None),
    ("nenhum/python-modulo-aider-tools", None),
    ("nenhum/grep-gemini", None),
];

const NEW_AGENTS: &[&str] = &[
    "gemini",
    "copilot",
    "kiro",
    "opencode",
    "aider",
    "goose",
    "cursor-agent",
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/adapters")
}

struct Case {
    comm: String,
    exe: String,
    cmdline: String,
}

fn load(case: &str) -> Case {
    let dir = fixtures().join(case);
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f)).unwrap_or_else(|e| panic!("{case}/{f}: {e}"))
    };
    let exe_target = read("exe");
    let exe = Path::new(exe_target.trim_end())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .trim_end_matches(" (deleted)")
        .to_string();
    Case {
        comm: read("comm").trim_end_matches('\n').to_string(),
        exe,
        cmdline: read("cmdline"),
    }
}

/// Detecta como o sampler fara, contando as leituras de `exe` e de cmdline.
fn detect(r: &Registry, c: &Case) -> (Option<String>, usize) {
    let cmdline_reads = Cell::new(0);
    let mut exe = || Some(c.exe.clone());
    let mut cmdline = || {
        cmdline_reads.set(cmdline_reads.get() + 1);
        Some(c.cmdline.clone())
    };
    let id = r
        .detect_with(&c.comm, &mut exe, &mut cmdline)
        .map(|a| a.id().to_string());
    (id, cmdline_reads.get())
}

#[test]
fn adapters_fixture_table_detects_the_right_agent() {
    let r = Registry::default();
    let mut wrong = Vec::new();
    for (case, want) in TABLE {
        let c = load(case);
        let (got, cmdline_reads) = detect(&r, &c);
        if got.as_deref() != *want {
            wrong.push(format!("{case}: esperado {want:?}, veio {got:?}"));
        }
        if interpreter(&c.comm).is_none() {
            assert_eq!(
                cmdline_reads, 0,
                "{case}: comm {:?} nao e interpretador e a cmdline foi lida",
                c.comm
            );
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn adapters_every_fixture_is_in_the_table_and_every_new_agent_has_one() {
    let mut on_disk = BTreeSet::new();
    for agent in std::fs::read_dir(fixtures()).unwrap() {
        let agent = agent.unwrap();
        for case in std::fs::read_dir(agent.path()).unwrap() {
            on_disk.insert(format!(
                "{}/{}",
                agent.file_name().to_string_lossy(),
                case.unwrap().file_name().to_string_lossy()
            ));
        }
    }
    let listed: BTreeSet<String> = TABLE.iter().map(|(c, _)| c.to_string()).collect();
    assert_eq!(
        on_disk, listed,
        "fixture sem linha na tabela, ou o contrario"
    );
    for id in NEW_AGENTS {
        assert!(
            TABLE.iter().any(|(_, want)| *want == Some(*id)),
            "{id} sem fixture positiva"
        );
    }
    let ids = Registry::default().ids().join(",");
    for id in NEW_AGENTS {
        assert!(ids.split(',').any(|x| x == *id), "{id} fora de builtin()");
    }
}

#[test]
fn adapters_negatives_cover_mcp_pnpm_and_pytest() {
    let r = Registry::default();
    for case in [
        "nenhum/node-context7-mcp",
        "nenhum/node-pnpm-run-test",
        "nenhum/python-pytest",
        "copilot/npm-loader",
    ] {
        let c = load(case);
        assert!(
            interpreter(&c.comm).is_some(),
            "{case} deveria ser node/python"
        );
        let (got, reads) = detect(&r, &c);
        assert_eq!(got, None, "{case} virou sessao");
        assert_eq!(reads, 1, "{case}: a cmdline tinha de ser consultada");
    }
}

#[test]
fn adapters_detect_without_cmdline_stays_comm_and_exe_only() {
    let r = Registry::default();
    let gemini = load("gemini/npm-bin");
    assert!(
        r.detect(&gemini.comm, &mut || Some(gemini.exe.clone()))
            .is_none()
    );
    let opencode = load("opencode/caminho-exe");
    let found = r
        .detect(&opencode.comm, &mut || Some(opencode.exe.clone()))
        .map(|a| a.id().to_string());
    assert_eq!(found.as_deref(), Some("opencode"));
}

#[test]
fn adapters_comm_match_wins_over_hosted() {
    // Extra com comm "node": vence a regra de argv do gemini.
    let mut adapters = agentcenter_core::adapters::builtin();
    adapters.insert(0, Box::new(parse_extra_agents("node").remove(0)));
    let r = Registry::new(adapters);
    let (got, reads) = detect(&r, &load("gemini/npm-bin"));
    assert_eq!(got.as_deref(), Some("node"));
    assert_eq!(reads, 0, "comm casou: nao ha por que ler a cmdline");
}

#[test]
fn adapters_unreadable_cmdline_is_no_match() {
    let r = Registry::default();
    let c = load("gemini/npm-bin");
    let found = r.detect_with(&c.comm, &mut || Some(c.exe.clone()), &mut || None);
    assert!(found.is_none());
}

#[test]
fn adapters_interpreter_comms() {
    for comm in ["node", "nodejs", "bun"] {
        assert_eq!(interpreter(comm), Some(Interpreter::Node), "{comm}");
    }
    for comm in ["python", "python3", "python3.12", "python3.9"] {
        assert_eq!(interpreter(comm), Some(Interpreter::Python), "{comm}");
    }
    for comm in [
        "python3.",
        "python3.x",
        "python2",
        "pythonw",
        "nodemon",
        "bash",
        "gemini",
        "",
    ] {
        assert_eq!(interpreter(comm), None, "{comm}");
    }
}

#[test]
fn adapters_hosted_parse_finds_the_target() {
    use Interpreter::{Node, Python};
    use Target::{Module, Script};
    let cases: &[(Interpreter, &str, Option<Target<'_>>)] = &[
        (Node, "node\0/a/gemini\0--x\0", Some(Script("/a/gemini"))),
        (
            Node,
            "node\0--require\0/p.cjs\0--import\0file:///l.mjs\0src/i.ts\0",
            Some(Script("src/i.ts")),
        ),
        (Node, "node\0-r\0/p.cjs\0/a/b.js\0", Some(Script("/a/b.js"))),
        (
            Node,
            "node\0--max-old-space-size=8\0/a\0",
            Some(Script("/a")),
        ),
        (Node, "node\0--\0-weird.js\0", Some(Script("-weird.js"))),
        (Node, "node\0-e\0require('/a/gemini')\0", None),
        (Node, "node\0", None),
        (Node, "", None),
        (Python, "python3\0-m\0aider\0", Some(Module("aider"))),
        (Python, "python3\0-mpytest\0", Some(Module("pytest"))),
        (Python, "python\0-W\0ignore\0-m\0a.b\0", Some(Module("a.b"))),
        (
            Python,
            "python3\0-u\0/bin/aider\0",
            Some(Script("/bin/aider")),
        ),
        (Python, "python3\0-c\0import aider\0", None),
        (Python, "python3\0-\0", None),
    ];
    for (interp, raw, want) in cases {
        assert_eq!(HostedCmd::parse(*interp, raw).target, *want, "{raw:?}");
    }
    let cmd = HostedCmd::parse(Node, "/x/agent\0/v/index.js\0");
    assert_eq!(cmd.argv0, "/x/agent");
    assert_eq!(cmd.script_name(), Some("index"));
    assert_eq!(
        HostedCmd::parse(Node, "node\0/b/gemini.mjs\0").script_name(),
        Some("gemini")
    );
}

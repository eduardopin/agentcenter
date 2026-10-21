//! Idade do ultimo handoff do projeto, copia de `contexto.latest_handoff`.
//!
//! Sobe do cwd ate o primeiro diretorio com `.git` (diretorio ou arquivo: uma
//! worktree para na propria raiz e nunca olha o repo pai), juntando os
//! `<dir>/.claude/handoffs/*.md` do caminho e os nomes dos diretorios. Sem
//! candidato, usa os `~/.claude/handoffs/*.md` cujo nome, em minusculas, contem
//! algum desses nomes (mais de 3 caracteres, `_` lido como `-`). Ganha o de
//! maior mtime.
//!
//! So lista diretorios e le metadados (`readdir` + `stat`): nenhum `.md` e
//! aberto, entao o conteudo de um handoff nunca entra no processo.
//!
//! Herdado do script, de proposito, e nao corrigido aqui: o casamento global e
//! por substring de qualquer diretorio do caminho, entao um cwd em
//! `/home/x/proj` (fora de repo) casa com um handoff global chamado
//! `home-...md` porque o diretorio `home` tem 4 letras.
//!
//! Diferencas conhecidas com o script, todas em casos que nele quebram ou sao
//! acidentais: um `.md` cujo `stat` falha (link quebrado) e ignorado, onde o
//! script levanta excecao; metacaracteres de glob (`[`, `*`, `?`) no caminho
//! sao literais aqui; nomes fora de UTF-8 sao comparados pelo
//! `to_string_lossy`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Um diretorio modificado menos de `RACY` antes da listagem pode mudar de novo
/// sem mudar o mtime (o relogio do sistema de arquivos e grosso): a listagem
/// dele nao e reaproveitada ate ficar mais velha que isso.
const RACY: Duration = Duration::from_secs(2);

unsafe extern "C" {
    safe fn geteuid() -> u32;
}

/// Diretorios de handoff guardados no maximo; acima disso sai a listagem usada
/// ha mais tempo (LRU). Cada cwd consultado guarda de 1 a 3 diretorios (os
/// `.claude/handoffs` da subida e o global), entao 256 cobre bem mais
/// projetos que as sessoes vivas de uma maquina.
pub const DIRS_CAP: usize = 256;

/// O handoff escolhido e a idade em minutos em relacao ao `now` da consulta
/// (negativa se o mtime estiver no futuro, como no script).
#[derive(Debug, Clone, PartialEq)]
pub struct Handoff {
    pub path: PathBuf,
    pub age_min: f64,
}

/// Listagem de um diretorio de handoffs, valida enquanto o mtime dele nao muda.
#[derive(Debug)]
struct Listing {
    mtime: SystemTime,
    read_at: SystemTime,
    names: Vec<OsString>,
    /// Valor de [`HandoffIndex::clock`] no ultimo uso: o LRU.
    used: u64,
}

/// Indice de handoffs com cache por diretorio: um `readdir` so quando o mtime
/// do diretorio muda; o `stat` de cada `.md` e feito em toda consulta (reescrever
/// um arquivo muda o mtime dele, nao o do diretorio). Pensado para o C21
/// consultar a cada 5 s. Guarda no maximo [`DIRS_CAP`] listagens.
#[derive(Debug, Default)]
pub struct HandoffIndex {
    dirs: HashMap<PathBuf, Listing>,
    readdirs: u64,
    /// Conta os usos de listagem; ordena o LRU.
    clock: u64,
}

impl HandoffIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Quantos `readdir` o indice ja fez.
    pub fn readdir_count(&self) -> u64 {
        self.readdirs
    }

    /// Listagens de diretorio guardadas agora (no maximo [`DIRS_CAP`]).
    pub fn cached_dirs(&self) -> usize {
        self.dirs.len()
    }

    /// Handoff mais recente do projeto de `cwd`; `home` e a raiz dos globais
    /// (`<home>/.claude/handoffs`) e `now` o relogio da idade.
    pub fn latest(&mut self, cwd: &Path, home: &Path, now: SystemTime) -> Option<Handoff> {
        self.search(cwd, Some(home), now, Path::new("/"))
    }

    /// [`latest`](Self::latest) com o `home` opcional: sem ele vale so o
    /// handoff local (os `.claude/handoffs` da subida ate o repo); o fallback
    /// global por nome e que precisa do `home`.
    pub fn find(&mut self, cwd: &Path, home: Option<&Path>, now: SystemTime) -> Option<Handoff> {
        self.search(cwd, home, now, Path::new("/"))
    }

    /// [`latest`](Self::latest) com `top` no papel do `/`: a subida para ao
    /// chegar nele, sem olhar o proprio `top` (nem o nome dele) nem acima. Com
    /// `top` = `/` e o mesmo que `latest`; um `top` fora do caminho do cwd nao
    /// limita nada. Os testes usam para que nenhum ancestral do diretorio
    /// temporario entre na busca.
    pub fn latest_below(
        &mut self,
        cwd: &Path,
        home: &Path,
        now: SystemTime,
        top: &Path,
    ) -> Option<Handoff> {
        self.search(cwd, Some(home), now, top)
    }

    /// AgentCenter: identidade exata no nome, nunca pelo cwd ou pelo conteudo.
    pub fn find_agentcenter(
        &mut self,
        dir: &Path,
        session_id: &str,
        now: SystemTime,
    ) -> Option<Handoff> {
        if session_id.is_empty() || session_id.len() > 90 {
            return None;
        }
        let prefix = format!(
            "s{}-",
            session_id
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let directory = fs::symlink_metadata(dir).ok()?;
        if !directory.is_dir() || directory.uid() != geteuid() {
            return None;
        }
        let names = self.listing(dir)?;
        let mut best = None;
        for name in names {
            let Some(name) = name.to_str().filter(|n| n.starts_with(&prefix)) else {
                continue;
            };
            let path = dir.join(name);
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_file() || meta.uid() != directory.uid() {
                continue;
            }
            let Ok(mtime) = meta.modified() else {
                continue;
            };
            if best.as_ref().is_none_or(|(_, old)| mtime > *old) {
                best = Some((path, mtime));
            }
        }
        let (path, mtime) = best?;
        let age_min = match now.duration_since(mtime) {
            Ok(d) => d.as_secs_f64() / 60.0,
            Err(e) => -e.duration().as_secs_f64() / 60.0,
        };
        Some(Handoff { path, age_min })
    }

    fn search(
        &mut self,
        cwd: &Path,
        home: Option<&Path>,
        now: SystemTime,
        top: &Path,
    ) -> Option<Handoff> {
        if cwd.as_os_str().is_empty() {
            return None;
        }
        let top = abspath(top);
        let mut cands: Vec<(PathBuf, SystemTime)> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        let mut probe = abspath(cwd);
        while probe != top
            && let Some(parent) = probe.parent()
        {
            let name = probe
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !names.contains(&name) {
                names.push(name);
            }
            self.collect(&probe.join(".claude").join("handoffs"), &mut cands, |_| {
                true
            });
            if fs::metadata(probe.join(".git")).is_ok_and(|m| m.is_dir() || m.is_file()) {
                break;
            }
            probe = parent.to_path_buf();
        }
        if let Some(home) = home.filter(|_| cands.is_empty() && !names.is_empty()) {
            let wanted: Vec<String> = names
                .iter()
                .filter(|n| n.chars().count() > 3)
                .map(|n| n.replace('_', "-"))
                .collect();
            let global = home.join(".claude").join("handoffs");
            self.collect(&global, &mut cands, |base| {
                let base = base.to_string_lossy().to_lowercase();
                wanted.iter().any(|n| base.contains(n.as_str()))
            });
        }
        // `max` do Python: no empate fica o primeiro.
        let mut best: Option<(PathBuf, SystemTime)> = None;
        for (path, mtime) in cands {
            if best.as_ref().is_none_or(|(_, m)| mtime > *m) {
                best = Some((path, mtime));
            }
        }
        let (path, mtime) = best?;
        let age_min = match now.duration_since(mtime) {
            Ok(d) => d.as_secs_f64() / 60.0,
            Err(e) => -e.duration().as_secs_f64() / 60.0,
        };
        Some(Handoff { path, age_min })
    }

    /// Junta os `*.md` de `dir` aceitos por `keep` (pelo nome), com o mtime.
    fn collect(
        &mut self,
        dir: &Path,
        out: &mut Vec<(PathBuf, SystemTime)>,
        keep: impl Fn(&OsString) -> bool,
    ) {
        let Some(names) = self.listing(dir) else {
            return;
        };
        for name in names.iter().filter(|n| keep(n)) {
            let path = dir.join(name);
            if let Ok(mtime) = fs::metadata(&path).and_then(|m| m.modified()) {
                out.push((path, mtime));
            }
        }
    }

    /// Os nomes `*.md` de `dir`, do cache se o mtime do diretorio nao mudou.
    fn listing(&mut self, dir: &Path) -> Option<&Vec<OsString>> {
        let Ok(mtime) = fs::metadata(dir).and_then(|m| m.modified()) else {
            self.dirs.remove(dir);
            return None;
        };
        self.clock += 1;
        let used = self.clock;
        let fresh = self.dirs.get(dir).is_some_and(|l| {
            l.mtime == mtime && l.read_at.duration_since(mtime).is_ok_and(|d| d >= RACY)
        });
        if !fresh {
            if self.dirs.len() >= DIRS_CAP && !self.dirs.contains_key(dir) {
                self.evict_lru();
            }
            let read_at = SystemTime::now();
            self.readdirs += 1;
            let names = fs::read_dir(dir)
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok().map(|e| e.file_name()))
                        .filter(is_md)
                        .take(4096)
                        .collect()
                })
                .unwrap_or_default();
            self.dirs.insert(
                dir.to_path_buf(),
                Listing {
                    mtime,
                    read_at,
                    names,
                    used,
                },
            );
        }
        let l = self.dirs.get_mut(dir)?;
        l.used = used;
        Some(&l.names)
    }

    /// Tira a listagem usada ha mais tempo (varredura linear: so roda quando
    /// o indice esta cheio e um diretorio novo chega).
    fn evict_lru(&mut self) {
        let oldest = self
            .dirs
            .iter()
            .min_by_key(|(_, l)| l.used)
            .map(|(dir, _)| dir.clone());
        if let Some(dir) = oldest {
            self.dirs.remove(&dir);
        }
    }
}

/// [`HandoffIndex::latest`] sem cache, para uma consulta avulsa.
pub fn latest_handoff(cwd: &Path, home: &Path, now: SystemTime) -> Option<Handoff> {
    HandoffIndex::new().latest(cwd, home, now)
}

/// O `*.md` do glob: termina em `.md` e nao comeca com `.` (oculto).
fn is_md(name: &OsString) -> bool {
    let b = name.as_encoded_bytes();
    b.len() > 3 && b.ends_with(b".md") && b[0] != b'.'
}

/// `os.path.abspath`: absoluto pelo diretorio atual e normalizado sem tocar o
/// disco (`..` remove o componente anterior, links nao sao resolvidos).
fn abspath(p: &Path) -> PathBuf {
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut out = PathBuf::from("/");
    for c in joined.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

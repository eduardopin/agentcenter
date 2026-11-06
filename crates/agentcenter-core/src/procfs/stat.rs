//! `/proc/<pid>/stat`.

use super::ReadError;
use super::statm::PAGE_SIZE;

/// Campos de `/proc/<pid>/stat` que o sampler usa. Tempos em ticks (USER_HZ).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub pid: u32,
    pub comm: String,
    pub state: char,
    pub ppid: u32,
    pub utime: u64,
    pub stime: u64,
    pub num_threads: u32,
    /// Campo 22: instante de inicio em ticks desde o boot; distingue reuso de pid.
    pub starttime: u64,
    /// Campo 23: memoria virtual em bytes. Zero so sem `mm` (zumbi, thread de kernel).
    pub vsize: u64,
    /// Campo 24: RSS em paginas, o mesmo contador de `VmRSS` no status e do
    /// campo resident de `statm`.
    pub rss_pages: u64,
    /// Campo 42: ticks esperando E/S de bloco. Zero se `task_delayacct` desligado.
    pub delayacct_blkio_ticks: u64,
}

/// Faz o parse de uma linha de `stat`. O `comm` pode conter espaco e `)`, entao
/// ele vai do primeiro `(` ao ULTIMO `)`; os demais campos vem depois.
pub fn parse_stat(line: &str) -> Result<Stat, ReadError> {
    let open = line
        .find('(')
        .ok_or_else(|| ReadError::parse("stat: no '('"))?;
    let close = line
        .rfind(')')
        .ok_or_else(|| ReadError::parse("stat: no ')'"))?;
    if close < open {
        return Err(ReadError::parse("stat: ')' before '('"));
    }
    let pid = line[..open]
        .trim()
        .parse::<u32>()
        .map_err(|_| ReadError::parse("stat: pid"))?;
    let comm = line[open + 1..close].to_string();
    // Uma passada so, sem juntar os ~50 campos num `Vec` (uma alocacao por
    // `stat` lido, centenas por tick): o primeiro item depois do `)` e o campo
    // 3 (state) e `field(n)` anda ate o campo `n`. Os campos sao pedidos em
    // ordem crescente, na mesma ordem em que os erros eram reportados antes.
    let mut rest = line[close + 1..].split_ascii_whitespace();
    let state = rest
        .next()
        .and_then(|s| s.chars().next())
        .ok_or_else(|| ReadError::parse("stat: state"))?;
    let mut next = 4; // numero do campo que `rest.next()` devolve agora
    let mut field = |n: usize| -> Result<u64, ReadError> {
        let tok = rest.nth(n - next);
        next = n + 1;
        tok.ok_or_else(|| ReadError::parse("stat: short line"))?
            .parse::<u64>()
            .map_err(|_| ReadError::parse("stat: number"))
    };
    Ok(Stat {
        pid,
        comm,
        state,
        ppid: field(4)? as u32,
        utime: field(14)?,
        stime: field(15)?,
        num_threads: field(20)? as u32,
        starttime: field(22)?,
        vsize: field(23)?,
        rss_pages: field(24)?,
        // Kernels muito antigos nao tem o campo 42; trata como zero.
        delayacct_blkio_ticks: field(42).unwrap_or(0),
    })
}

impl Stat {
    /// RSS em bytes. `None` sem `mm` (zumbi, thread de kernel): o kernel escreve
    /// 0 em `vsize` e em `rss`, e o status nem tem a linha `VmRSS`. Processo com
    /// tudo em swap tem `vsize` e da `Some(0)`, que e uma medida.
    pub fn rss_bytes(&self) -> Option<u64> {
        (self.vsize != 0 || self.rss_pages != 0).then(|| self.rss_pages * PAGE_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// O parser anterior (campos juntados num `Vec`), guardado como oraculo: a
    /// passada unica tem de devolver o mesmo `Ok` e o mesmo erro, linha a linha.
    fn parse_stat_vec(line: &str) -> Result<Stat, ReadError> {
        let open = line
            .find('(')
            .ok_or_else(|| ReadError::parse("stat: no '('"))?;
        let close = line
            .rfind(')')
            .ok_or_else(|| ReadError::parse("stat: no ')'"))?;
        if close < open {
            return Err(ReadError::parse("stat: ')' before '('"));
        }
        let pid = line[..open]
            .trim()
            .parse::<u32>()
            .map_err(|_| ReadError::parse("stat: pid"))?;
        let comm = line[open + 1..close].to_string();
        let rest: Vec<&str> = line[close + 1..].split_ascii_whitespace().collect();
        let field = |n: usize| -> Result<u64, ReadError> {
            rest.get(n - 3)
                .ok_or_else(|| ReadError::parse("stat: short line"))?
                .parse::<u64>()
                .map_err(|_| ReadError::parse("stat: number"))
        };
        let state = rest
            .first()
            .and_then(|s| s.chars().next())
            .ok_or_else(|| ReadError::parse("stat: state"))?;
        Ok(Stat {
            pid,
            comm,
            state,
            ppid: field(4)? as u32,
            utime: field(14)?,
            stime: field(15)?,
            num_threads: field(20)? as u32,
            starttime: field(22)?,
            vsize: field(23)?,
            rss_pages: field(24)?,
            delayacct_blkio_ticks: field(42).unwrap_or(0),
        })
    }

    /// Os `n` primeiros campos a partir do 3 (o estado, `S`); o campo N vale `N * 10`.
    fn tokens(n: usize) -> Vec<String> {
        (3..3 + n)
            .map(|f| {
                if f == 3 {
                    "S".into()
                } else {
                    (f * 10).to_string()
                }
            })
            .collect()
    }

    fn line(comm: &str, toks: &[String]) -> String {
        format!("4242 ({comm}) {}\n", toks.join(" "))
    }

    #[test]
    fn parse_stat_reads_the_fields_the_sampler_uses() {
        let st = parse_stat(&line("claude", &tokens(50))).unwrap();
        assert_eq!(st.pid, 4242);
        assert_eq!(st.comm, "claude");
        assert_eq!(st.state, 'S');
        assert_eq!(st.ppid, 40);
        assert_eq!((st.utime, st.stime), (140, 150));
        assert_eq!(st.num_threads, 200);
        assert_eq!(st.starttime, 220);
        assert_eq!((st.vsize, st.rss_pages), (230, 240));
        assert_eq!(st.delayacct_blkio_ticks, 420);
    }

    #[test]
    fn parse_stat_matches_the_vec_parser_on_every_truncation_and_bad_field() {
        let comms = ["bash", "a b", "x) (y", "(", ")", "", "kworker/u32:1"];
        let mut cases: Vec<String> = Vec::new();
        for comm in comms {
            // Linha cortada em cada tamanho, inclusive sem nenhum campo.
            for n in 0..=50 {
                cases.push(line(comm, &tokens(n)));
            }
            // Cada campo lido (e os vizinhos) trocado por lixo, vazio ou negativo.
            for bad in ["x", "-1", "1x", "99999999999999999999999"] {
                for f in 3..=44 {
                    let mut t = tokens(50);
                    t[f - 3] = bad.to_string();
                    cases.push(line(comm, &t));
                }
            }
        }
        cases.extend(
            [
                "",
                "no parens 1 2 3",
                "1 ) x (",
                "x (a) S 1",
                "  7 (a)  S  1\t2\n3 ",
                "7 (a)",
                "7 (a) ",
            ]
            .map(String::from),
        );
        // Espacos e tabs entre os campos, e sem quebra de linha no fim.
        cases.push(
            line("t", &tokens(50))
                .replace(' ', " \t  ")
                .trim_end()
                .into(),
        );
        for c in &cases {
            assert_eq!(parse_stat(c), parse_stat_vec(c), "{c:?}");
        }
        assert!(cases.len() > 700);
    }

    #[test]
    fn parse_stat_without_field_42_counts_zero_stall() {
        let st = parse_stat(&line("old", &tokens(30))).unwrap();
        assert_eq!(st.delayacct_blkio_ticks, 0);
        assert_eq!(st.rss_pages, 240);
    }
}

# Pendências do AgentCenter

Estado conferido em 22/10/2026. Repositório público criado:
https://github.com/eduardopin/agentcenter

Este repositório disponibiliza um snapshot de desenvolvimento da fonte MIT.
Não há release estável ou binários publicados. Esta lista não representa
aprovação funcional da app; os gates abaixo continuam abertos.

O snapshot de 22/10 foi integrado por decisão do dono com stories ainda sem
aceite integral: controle Pi (CC1), dashboard (CC2), botões (CC3), Claude Code
via Herdr (CC4), gauges por linha (CC8) e gráficos de cockpit no detalhe (CC9).

## Pendências para release e aceite funcional

- [ ] CC1: a revisão extra autorizada pelo dono deu FIX_FIRST; os dois
  bloqueadores (fixture sem `~/.cache` no runner limpo e teste de nonce errado
  bem formado) foram corrigidos só em testes, sem nova revisão independente.
  Avisos abertos: mutações sobreviventes em limites de conexão/frame/idle,
  fingerprint de duplicata, re-checagem pós-`link` e starttime errado; teste
  instável em `operations.test.mjs:73`; testes Node com caminhos fixos fora do
  CI; `herdr_tests.rs` deixa diretório para trás quando o bind falha.
- [ ] Obter aceite independente do retorno explícito a Reclaim (CC2).
- [ ] CC3: a regra "controle sem capacidade real não aparece" e a prova de
  que tick estável não redescobre a sessão não têm revisão independente. A
  CPU de 12,198% não se reproduziu (0,20% sem detalhe; 7,7–9,0% com detalhe
  aberto em CC2 e CC3); remedir em série com máquina quieta.
- [ ] CC4 (Claude Code via Herdr): FIX_FIRST corrigido (hold de `unknown`
  contado da transição; claim ilegível falha fechado), sem nova revisão. Falta
  E2E com Claude real isolado, teste de ausência de conteúdo no Snapshot,
  `agent.send_keys Escape`, `scripts/ci.sh` e medição. O socket Herdr real é
  recusado enquanto `~/.config/herdr` tiver escrita de grupo.
- [ ] CC8 (gauges por linha): SHIP do código; faltam medição GUI antes/depois,
  `scripts/ci.sh`, capturas `vazio`/`uma` e das densidades larga/compacta.
- [ ] CC9 (gráficos de cockpit): FIX_FIRST sem defeito de lógica. Faltam teste
  de fiação visual de marcas e barras de loop em `Strip::set_marked`, capturas
  (overflow intermitente em 360 no Wayland, anel de foco), leitura dupla do
  resumo pelo leitor de tela, medição de tick/GUI e textos de DESIGN.md e
  ARCHITECTURE.md. O clone do histórico por tick cresceu cerca de 2,2x.
- [ ] Testes instáveis. `cadence::spawned_sampler_delivers_frames_by_channel_and_stops_on_drop`
  falhou uma vez com load > 50 (ritmo 263 ms < 280 ms) e passou 3/3 isolado.
  Os dois `window::qa_tests::cc2_native_reclaim_*` falham de forma
  intermitente na asserção da rolagem do Reclaim (`window_qa_tests.rs:140`,
  valor 1 a 19 em vez de 0): cerca de 1 em 4 execuções nas árvores integradas,
  0 em 25 na base de 21/10 e 0 em 10 na base com o mesmo código de GUI de
  22/10. Causa não demonstrada; não há fonte de dados vivos no teste.
- [ ] Corte de 22/10 (tarde), integrado por decisão do dono ("faça um corte
  na aplicação como ela está") com aceites incompletos:
  - CC6/CC6b (handoff-pickup do Claude Code via Herdr): SHIP na revisão
    rodada 2, inclusive E2E com Claude Code real em Herdr isolado
    (`scripts/e2e-claude-herdr.sh`). Avisos abertos: comportamentos sem teste
    dedicado (unknown por resposta perdida no prompt do pickup, argumento
    exato do pickup, `Env::new` lendo `XDG_STATE_HOME`), comando do E2E não
    citado no CONTRIBUTING, `unsafe_copy_path` só recusado depois do turno de
    Handoff. Em sessão real, o Write do handoff em `<repo>/.claude/handoffs`
    pede confirmação do Claude Code se a sessão ainda não liberou a pasta
    `.claude`; o Handoff então termina `unknown` pelo prazo.
  - CC5/CC5b/CC5c (handoff-pickup do Pi via Herdr, decisão do dono 22/10):
    FIX_FIRST na rodada 2 e integrado como está por decisão do dono. Defeitos
    conhecidos, ambos falham para o lado seguro sem perder dado: o manifesto
    fica `busy` depois de um arme que não virou sessão nova, até o próximo
    evento de agente do Pi; e um segundo arme depois do prazo do `newSession`
    pode criar duas sessões novas se a troca antiga destravar. A identidade
    do Pi no Herdr por arquivo de sessão (`kind=path`) entrou sem revisão
    independente; o E2E com Pi real e o teste cruzado Node→Rust não foram
    feitos.
  - CC7/CC7b (seção de controle): FIX_FIRST na rodada 1 corrigido (motivo
    sumindo, redescoberta escondendo a seção e o foco), rodada 2 sem
    veredito no corte. "Handoff + nova sessão" segue escondido no app
    (`pickup_not_integrated`) até ligar os adaptadores reais do CC5/CC6; o
    E2E GUI → adaptador real não existe. Abertos: `herdr_server` e o
    construtor de `Session` sem teste, `CLAUDE_CONFIG_DIR` ignorado pela GUI,
    capacidade que cai sem mudança de status no Herdr só percebida no foco
    da janela, critério de Herdr diferente do `detail.rs`, captura real em
    1280x800 (o Broadway para em 1024x768) e medição da GUI com máquina
    parada. Em GTK 4.22.4 `gtk_accessible_announce` sem barramento de
    acessibilidade derruba o processo; o anúncio só é chamado nos contextos
    AT-SPI/AccessKit.
  - CC10 (tick d_state): `scripts/ci.sh` do corte passou o bench gate com
    mediana d_state de 4,014 ms (1,25x de folga; antes 5,036–5,111 ms),
    claude_40 2,939 ms e delayacct 2,757 ms, com load 3–7 e sem máquina
    parada; a folga-alvo de 1,3x não foi atingida. FIX_FIRST leve na revisão, não
    corrigido: `task/` aberto sem `O_DIRECTORY` (uma FIFO numa raiz de fixture
    trava a varredura; não ocorre em `/proc` real), falta teste de vazamento
    de fd e de `O_CLOEXEC`, teste de alocação falha com `TMPDIR` maior que
    ~205 caracteres, comparação de caminho por `Path` e não por bytes, buffer
    do `getdents64` sem alinhamento explícito. Exige glibc 2.30 ou mais nova.
- [ ] Fazer o gate integral passar sem afrouxar testes ou orçamentos.
  O último gate local medido falhou: mediana D-state de 5,036 ms, limite <5 ms.
- [ ] Resolver os gates da GUI. As últimas medições registraram:

  | Árvore | Primeiro frame, limite <300 ms | CPU, limite <2% de um core | PSS, limite <80 MiB |
  |---|---:|---:|---:|
  | Base | 501,8 ms | 0,150% | 46,1 MiB |
  | CC2 | 430,3 ms | 0,167% | 46,2 MiB |
  | CC3 | 456,4 ms | 12,198% | 50,1 MiB |

  A base também falhou no primeiro frame. A causa das diferenças não foi
  demonstrada; esses números não autorizam dispensar os gates.
- [ ] Completar os negativos GUI → socket → extensão: rejeição/quota,
  cancelamento durante polling, timeout, consulta pelo mesmo ID e resultado
  tardio. Testes de modelo e estado GTK não substituem esse caminho completo.
- [ ] Completar teclado/Tab, foco assíncrono e retorno ao cockpit; comprovar
  leitura integral da confirmação por rolagem em 360 px e a matriz visual,
  incluindo escala de texto e estados vazio/uma/40 sessões/nomes longos.
- [ ] Realizar UAT em sessões reais somente com autorização separada.

## Preparação e envio

- [x] Criar o repositório público e configurar `origin`.
- [x] Disponibilizar a fonte MIT auditada com esta lista de pendências,
  sem ancestralidade privada ou binários.
- [x] Gerar build release local da GUI e CLI; smoke funcional passou.
- [x] Preparar licença MIT e fonte limpa em `release/open-source-mit`.
- [x] Auditar privacidade, licenças e integridade da exportação de fonte.
  O parecer é restrito à exportação, não ao funcionamento da aplicação.
- [ ] Concluir o aceite funcional do candidato, preservando WIP e autoria.
- [ ] Manter os próximos envios saneados e sujeitos aos gates aplicáveis.
  Não enviar o histórico antigo, que contém metadados internos, nem `.agenfk`,
  sessões, logs, `target`, `node_modules` ou runtime gerado. Não usar push
  `--all`/`--mirror`.
- [x] Validar o clone público, checksums e ausência de histórico privado.
- [x] Corrigir SC2115 no teste de instalação e confirmar ShellCheck e suíte
  do instalador, sem desabilitar o lint ou enfraquecer as verificações.
- [ ] Confirmar build e CI completos a partir do clone/remoto.
- [ ] Antes de distribuir binários, remover paths locais do compilador e
  empacotar as licenças e avisos das dependências efetivamente distribuídas.
  Os binários atuais são apenas locais.

## Evidências locais, não publicadas

- `target/cc123-resume-status-20261021.md`: testes, revisões e gates CC1/2/3.
- Worktree `open-source-mit`, `target/PUBLICATION_STATUS.md`: build e preparo MIT.
- Na mesma worktree, `target/publication-export-independent.md`: auditoria
  independente da fonte exportada; `target/agentcenter-source-mit.tar.gz` é o
  arquivo preparado. Esses arquivos não estão no GitHub.

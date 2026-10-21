# Pendências do AgentCenter

Estado conferido em 21/10/2026. Repositório público criado:
https://github.com/eduardopin/agentcenter

Este repositório disponibiliza um snapshot de desenvolvimento da fonte MIT.
Não há release estável ou binários publicados. Esta lista não representa
aprovação funcional da app; os gates abaixo continuam abertos.

## Pendências para release e aceite funcional

- [ ] Resolver com o dono o teto nativo de revisões do CC1 no Pi. A última
  revisão foi negada; não repetir por outro agente ou card, nem autoaprovar.
- [ ] Obter aceite independente das últimas correções do controle Pi (CC1)
  e do retorno explícito a Reclaim (CC2), respeitando esse controle nativo.
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

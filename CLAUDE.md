# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Projeto em pt-BR: código comentado, mensagens de log/CLI, commits, README e artefatos OpenSpec
são escritos em português (nos specs OpenSpec, títulos estruturais e SHALL/MUST ficam em inglês).

## Comandos

```bash
cargo build --workspace
cargo test --workspace                                   # tudo (provedor em memória, sem rede)
cargo test -p omarchy-drive-core <filtro>                # um teste ou grupo, ex.: planner::
cargo test -p omarchy-drive-core --test engine <nome>    # um teste ponta a ponta do motor
cargo test -p omarchy-drive-onedrive --test contract     # contrato do Graph com fixtures JSON
cargo test -p omarchy-drive-core --release bench_ -- --ignored --nocapture   # benchmarks de varredura
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
OMARCHY_DRIVE_LOG=debug cargo run -p omarchy-drive -- daemon
./install.sh                                             # build release + instala binário, unidade systemd e widget
```

`/check` (skill do projeto) roda fmt, clippy, testes e `qmllint` do widget de uma vez.
Um hook formata cada `.rs` editado com rustfmt; outro bloqueia edição de `packaging/oauth.env`
(client OAuth real, fora do git) e de `Cargo.lock`.

## Arquitetura

Workspace Rust (edition 2024) com quatro crates:

- `core`: tudo que não depende de API específica. Modelo, trait `Provider`, planejador,
  estado SQLite, motor, varredura local, OAuth (PKCE + loopback), HTTP com retry, chaveiro.
- `provider-gdrive` / `provider-onedrive`: implementações de `Provider`
  (Drive v3 `changes.list` + md5; Graph `delta` + quickXorHash + upload session).
- `app`: binário `omarchy-drive` = CLI (clap) + daemon + cliente do socket de controle.

### Ciclo de sincronização (por par local↔remoto)

`PairEngine` (`core/src/engine.rs`) executa um ciclo:

1. Aplica as mudanças remotas incrementais (cursor) à árvore remota id→pai/nome salva no SQLite
   (`state.rs`, `tree.rs`). Cursor invalidado (`ChangeBatch` com reset) força relistar tudo.
   Ordem importa: `start_cursor` é obtido **antes** de `list_tree` para não perder mudanças.
2. Varre a pasta local (`scan.rs`). `ScanScope` decide entre varredura completa e parcial
   (só caminhos reportados pelo inotify). Hash só é recalculado se tamanho ou mtime mudaram.
3. `planner::plan` é uma **função pura** sobre um `Snapshot` com três estados: base (último
   sincronizado), local e remoto. Gera `Action`s: upload/download, rename/move (por id remoto e
   inode local, sem retransferir), conflitos e exclusões. Toda a lógica de decisão mora aqui e
   é testada sem I/O.
4. O motor executa as ações em paralelo e re-confere o estado antes de qualquer passo destrutivo.

O daemon (`app/src/daemon.rs`) roda um `pair_loop` por par (inotify via `notify`, timers,
comandos), expõe o socket Unix `$XDG_RUNTIME_DIR/omarchy-drive/control.sock` (0600; protocolo
em `app/src/ipc.rs`) e grava `status.json` no mesmo diretório via `StatusHub`. Comandos de CLI
falam com o daemon pelo socket; `sync` roda o ciclo localmente se o daemon estiver parado.

O widget (`shell-plugin/`, QML do Quickshell/Omarchy) só **lê** `status.json` (tratado como dado
não confiável em `Model.js`) e dispara ações executando o próprio CLI. Mudanças no formato de
`StatusDoc` exigem manter compatibilidade (há campo `version`, hoje 2) e atualizar o widget.

### Invariantes de segurança (cobertas por testes em `core/tests/engine.rs`; não quebrar)

- Edição nos dois lados → mantém as duas versões (`conflict_name`: `arquivo (conflito host data).ext`).
- Editado de um lado e apagado do outro → a edição vence.
- Exclusões vão para lixeira (freedesktop local, lixeira do provedor remota), nunca apagam direto.
- Acima de `max_deletes_without_confirm` (padrão 50) exclusões ficam retidas até `confirm <par>`.
- Pasta local ausente/ilegível aborta o ciclo. Falha de leitura, arquivo em escrita (mtime < 2 s)
  ou pasta ilegível **nunca** viram "apagado".
- Download vai para arquivo temporário, é conferido pelo hash e renomeado atomicamente; aborta se
  o local mudou no meio.

### Outros pontos não óbvios

- Google Docs/Planilhas/Apresentações viram atalhos `.gdoc`/`.gsheet`/`.gslides` (`shortcut.rs`):
  renomear/mover/apagar o atalho propaga; editar o conteúdo é revertido.
- `Provider::validate_name` permite ao provedor recusar nomes (ex.: `:` no OneDrive); o planejador
  (`plan_checked`) pula o item e gera aviso em vez de falhar.
- Tokens: Secret Service via `oo7` (`token_store.rs`), com fallback para arquivo 0600 só quando não
  há Secret Service. Chaveiro bloqueado deixa os pares offline; nunca cair para arquivo nem abrir
  prompt de senha.
- O client OAuth padrão é embutido em tempo de compilação via `option_env!`
  (`OMARCHY_DRIVE_GOOGLE_CLIENT_ID`, `..._SECRET`, `OMARCHY_DRIVE_MS_CLIENT_ID`, carregados de
  `packaging/oauth.env` pelo `install.sh`; `app/build.rs` recompila quando mudam). Cada conta
  guarda o `client_id` que a autorizou em `config.toml`.
- Caminhos: config em `~/.config/omarchy-drive/config.toml`, estado em `~/.local/state/omarchy-drive/`,
  runtime (socket, status.json) em `$XDG_RUNTIME_DIR/omarchy-drive/` (`config::Dirs`).

## Testes

`core/tests/engine.rs` tem um `MemProvider` completo (com flags para simular token expirado,
cursor invalidado e nomes recusados) usado nos testes ponta a ponta; novos cenários do motor devem
usá-lo. Os provedores reais têm só testes de contrato com fixtures (`provider-onedrive/tests/`);
ainda não há testes contra as APIs reais.

## Planejamento

Mudanças maiores seguem OpenSpec (`openspec/changes/<nome>/`: proposal, design, specs, tasks).
A mudança ativa é `proximos-passos-paridade`; use as skills `openspec-*` para propor, aplicar e
arquivar.

# omarchy-drive

Sincronização bidirecional de **Google Drive** e **OneDrive** para o Omarchy, em Rust.
É um espelho local no estilo Insync: pastas reais no disco, sincronizadas nos dois sentidos,
com daemon systemd, CLI e widget na barra do Omarchy.

## Arquitetura

```
crates/
  core/               modelo, trait Provider, planejador de 3 vias, estado SQLite,
                      motor, OAuth (PKCE + loopback), HTTP com retry
  provider-gdrive/    Google Drive API v3 (changes.list, upload resumable, md5)
  provider-onedrive/  Microsoft Graph (delta, upload session, quickXorHash)
  app/                binário `omarchy-drive`: CLI + daemon + socket de controle
shell-plugin/         widget Quickshell da barra (lê status.json)
packaging/            unidade systemd --user
```

**Como um ciclo funciona:**

1. Busca as mudanças remotas de forma incremental (cursor de `changes`/`delta`) e
   atualiza a árvore remota (id → pai/nome) salva em SQLite.
2. Varre a pasta local. O hash só é recalculado quando o tamanho ou o mtime mudam.
3. O **planejador** (`core/src/planner.rs`, função pura) compara três estados: a **base**
   (último estado sincronizado), o **local** e o **remoto**. Ele decide:
   upload/download, renames e moves (por id remoto e por inode local, sem retransferir),
   conflitos e exclusões.
4. O motor executa as ações, com transferências em paralelo, e confere tudo de novo antes
   de qualquer passo destrutivo.

**Garantias de segurança** (cobertas por testes):

- Se os dois lados editaram, as **duas versões são mantidas**: `arquivo (conflito host data).ext`.
- Se um lado editou e o outro apagou, a edição vence e o arquivo é restaurado.
- Exclusões vão para a lixeira: freedesktop no local, lixeira do provedor no remoto.
- **Exclusão em massa** (acima de `max_deletes_without_confirm`, padrão 50) fica retida até
  rodar `omarchy-drive confirm <par>`.
- Se a pasta local está ausente ou ilegível (disco desmontado), o ciclo é abortado. Isso nunca
  vira "apagar tudo no remoto".
- Arquivos sendo escritos (mtime < 2 s), pastas ilegíveis e erros de leitura ficam **fora** do
  ciclo. Eles nunca são tratados como apagados.
- Downloads são gravados num arquivo temporário, conferidos pelo hash e só então renomeados
  atomicamente. Se o arquivo local mudar no meio, o download é abortado.

## Instalação

```bash
./install.sh
```

O script compila em release e instala `~/.local/bin/omarchy-drive`, a unidade systemd do
usuário e o plugin em `~/.config/omarchy/plugins/omarchy-drive.status`.

## Credenciais OAuth (uma vez)

Os apps pagos usam um client OAuth próprio, já verificado pelo Google e pela Microsoft.
Aqui você usa o seu.

**Client padrão (opcional).** Quem compila pode embutir um client no binário: copie
`packaging/oauth.env.example` para `packaging/oauth.env` (fora do git), preencha e rode
`./install.sh`. Com ele, "Conectar Google Drive" no painel funciona sem `setup`. Um client
configurado com `setup` tem prioridade para contas novas, e cada conta continua usando o client
que a autorizou (campo `client_id` da conta no `config.toml`).

**Google Drive**

1. Acesse <https://console.cloud.google.com/>, crie um projeto e ative a **Google Drive API**.
2. Em *Google Auth Platform → Público-alvo*, adicione o seu e-mail como usuário de teste.
   Num app não publicado, o refresh token **expira em 7 dias** (renove com
   `omarchy-drive account reauth <conta>`). Para uso contínuo, publique
   o app ("Em produção"). Sem verificação, ele funciona para até 100 usuários, com um aviso
   de "app não verificado".
3. Em *Clientes → Criar cliente*, escolha o tipo **App para computador**.
4. `omarchy-drive setup google --client-id <id> --client-secret <secret>`

**OneDrive**

1. Acesse <https://entra.microsoft.com/> → *Registros de aplicativo → Novo registro*.
   Em tipos de conta, escolha "Contas em qualquer diretório organizacional e contas pessoais
   da Microsoft".
2. Em *Autenticação*, adicione a plataforma **Aplicativos móveis e da área de trabalho** com o
   redirect `http://localhost` e ative **Permitir fluxos de clientes públicos**.
3. `omarchy-drive setup microsoft --client-id <id do aplicativo>`

## Uso

```bash
omarchy-drive account connect --provider gdrive         # abre o navegador; nome vem do e-mail
omarchy-drive account add trabalho --provider onedrive   # o mesmo, escolhendo o nome
omarchy-drive account reauth pessoal                     # token vencido/revogado; pares intactos
omarchy-drive account cancel                             # cancela uma autorização em andamento
omarchy-drive account remove pessoal --with-pairs        # remove conta e pares; arquivos ficam

omarchy-drive pair add drive    --account pessoal  --local ~/GoogleDrive
omarchy-drive pair add docs     --account trabalho --local ~/OneDrive/Docs --remote Documentos
omarchy-drive pair add fotos    --account pessoal  --local ~/Fotos --remote Fotos --download-only
omarchy-drive pair add seletivo --account pessoal  --local ~/Drive2 --include Projetos --include Notas

systemctl --user enable --now omarchy-drive
omarchy bar put omarchy-drive.status --section right

omarchy-drive status            # ou --json
omarchy-drive sync [par]        # força um ciclo (roda local se o daemon estiver parado)
omarchy-drive pause|resume [par]
omarchy-drive conflicts [--clear]
omarchy-drive confirm <par>     # libera exclusão em massa retida
journalctl --user -u omarchy-drive -f
```

**Na barra:** clique esquerdo abre o painel, clique direito sincroniza, clique do meio
pausa ou retoma. No painel, as teclas são `S` (sincronizar), `P` (pausar/retomar),
`O` (abrir pasta) e `C` (confirmar exclusões retidas).

Configuração: `~/.config/omarchy-drive/config.toml`. Tokens: `~/.local/state/omarchy-drive/tokens/`
(arquivos com permissão 0600).

## Desenvolvimento

```bash
cargo test --workspace     # planejador, hashes, estado e motor ponta a ponta com provedor em memória
cargo clippy --workspace --all-targets
OMARCHY_DRIVE_LOG=debug cargo run -p omarchy-drive -- daemon
```

## Roadmap: paridade com os apps pagos

| Recurso | Status |
|---|---|
| Sync bidirecional, vários pares e várias contas | ✅ |
| Detecção de rename/move sem retransferir | ✅ |
| Conflitos mantendo as duas versões | ✅ |
| Sync seletivo (`--include`), padrões de ignore, somente download | ✅ |
| Lixeira local e remota, proteção contra exclusão em massa | ✅ |
| Upload resumable/chunked, retry com backoff, renovação de token | ✅ |
| Daemon systemd, inotify, notificações de desktop, widget na barra | ✅ |
| **Teste contra as APIs reais** (só há testes com provedor em memória) | ⏳ próximo passo |
| Tokens no keyring (Secret Service / gnome-keyring) em vez de arquivo | ⏳ |
| Google Docs/Sheets/Slides como atalhos `.gdoc` ou exportação para .docx/.xlsx | ⏳ (hoje são ignorados) |
| Drives compartilhados (Google) e SharePoint/bibliotecas (OneDrive) | ⏳ |
| Limite de banda e pausa em rede medida | ⏳ |
| Varredura local incremental pelo inotify (hoje: varredura completa a cada ciclo) | ⏳ |
| Emblemas no gerenciador de arquivos (Nautilus) e "copiar link" no menu de contexto | ⏳ |
| Arquivos sob demanda via FUSE (estilo ExpanDrive) | ⏳ fase 2 |
| Histórico de versões e "restaurar versão" pela CLI | ⏳ |

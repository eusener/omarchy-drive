# Design

## Context

Estado atual, que motiva as decisões abaixo (para o porquê da mudança, ver proposal.md):

- **OAuth** (`core/src/oauth.rs`): o fluxo é `authorize_interactive`, com loopback em `127.0.0.1:0`, PKCE e `xdg-open`. Só a CLI o chama (`account add` e `account reauth`). Os tokens ficam em `~/.local/state/omarchy-drive/tokens/<conta>.json` (0600). O `TokenManager` lê esse arquivo e regrava a cada renovação.
- **Daemon** (`app/src/daemon.rs`): é um serviço systemd do usuário. Em `Reload`, recria os provedores, o que relê os tokens. Ele já usa `notify-send`. O socket aceita `SyncNow`, `Pause`, `Resume`, `ConfirmDeletes`, `ClearConflicts` e `Reload`.
- **Falhas de renovação**: viram um erro genérico (`PairState::Error`, com o texto em `last_error`). O widget não sabe distinguir "login expirado" de "sem rede".
- **Widget** (`shell-plugin/Panel.qml`): só lê `status.json` e chama a CLI com `Quickshell.execDetached`. O processo da CLI herda o ambiente gráfico da sessão.
- **Google Drive**: os arquivos nativos chegam com `native = true` e o motor os descarta (`engine.rs: remote.retain(|_, r| !r.item.native)`).
- **Varredura** (`core/src/scan.rs`): percorre a pasta inteira a cada ciclo. O daemon já filtra os eventos do inotify (`is_relevant`), mas joga fora os caminhos.
- **Credenciais**: `endpoints(cfg, kind)` lê o client de `[oauth]` no `config.toml` e falha se ele não estiver configurado.

## Goals / Non-Goals

**Goals:**
- O widget nunca precisa de terminal para conectar, reautorizar ou desconectar.
- Um único caminho de código para o OAuth, usado pela CLI e pelo widget.
- Cada item pode ser entregue e commitado separadamente, na ordem de tasks.md.

**Non-Goals:**
- Janela própria (app Qt): o widget da barra é a interface.
- Configurar pares pela interface gráfica. Entra um par padrão opcional no primeiro login (ver Decisão 3), mas editar pares continua na CLI.
- Exportar Docs para .docx/.xlsx.
- Watcher que não seja inotify.

## Decisions

### 1. O fluxo OAuth roda num processo da CLI disparado pelo widget, não dentro do daemon

O widget executa `omarchy-drive account connect --provider gdrive` (ou `reauth <conta>`) com `execDetached`. O processo da CLI faz o loopback, abre o navegador, grava os tokens e manda `Reload` ao daemon. O progresso aparece num arquivo `$XDG_RUNTIME_DIR/omarchy-drive/auth.json`, com estes campos:
- `{state: authorizing|done|failed|cancelled, provider, account?, message, pid, started_at}`;
- `flock` no próprio arquivo, que garante um único fluxo por vez.

O widget já observa a pasta de runtime e passa a ler também esse arquivo. "Cancelar" executa `omarchy-drive account cancel`, que manda SIGTERM ao `pid` registrado e marca `cancelled`. O tempo limite de 5 minutos fica com a própria CLI (`tokio::time::timeout` em volta do accept).

- *Alternativa considerada: OAuth no daemon, via novas requisições no socket.* Foi descartada porque o daemon roda sob systemd: o `xdg-open` dele depende de `WAYLAND_DISPLAY` e do ambiente importado para o gerenciador do usuário, o que é frágil. Além disso, um accept pendurado ficaria preso no processo de longa duração. O processo da CLI disparado pelo widget herda o ambiente correto e morre ao terminar.
- *Consequência:* `ipc.rs` não ganha novas requisições de conta. Basta o `Reload` que já existe.

### 2. Estado de autenticação tipado de ponta a ponta

- O `TokenManager` passa a devolver um erro tipado, `AuthError::Expired`, quando a renovação responde `invalid_grant` (Google) ou `invalid_grant` / `interaction_required` (Microsoft). O motor converte esse erro em um novo `PairState::AuthExpired`.
- O laço do par para de ciclar nesse estado e só volta a tentar no `Reload` (que acontece depois de reauth ou connect).
- O `StatusDoc` ganha `accounts: BTreeMap<String, AccountStatus{provider, label, auth: ok|expired}>`. O estado `authorizing` vem do `auth.json`, não do daemon.
- A versão do `status.json` sobe para 2. O widget aceita as versões 1 e 2.

### 3. Comando `account connect` sem nome

O comando `account connect --provider <p>`:
1. faz o OAuth;
2. descobre o e-mail;
3. procura uma conta do mesmo provedor com esse e-mail. Se existir, vira reauth. Se não, gera o nome com a regra da spec (parte local do e-mail normalizada, com sufixo `-2`, `-3`... se o nome já existir).

`account add <nome>` continua existindo para quem quer escolher o nome.

Quando a conta conectada é a primeira e não há nenhum par, o painel mostra o botão "Sincronizar Meu Drive em ~/GoogleDrive" (ou ~/OneDrive). Ele chama `pair add` com valores padrão. É um único clique, sem formulário.

### 4. Desconectar remove os pares junto, com confirmação no painel

`account remove <conta> --with-pairs` remove os pares da conta (configuração e banco SQLite do par), depois a conta e o segredo. Sem a flag, a CLI mantém o comportamento atual: recusa se ainda houver pares. O painel mostra uma linha de confirmação inline ("Desconectar e parar de sincronizar: drive, fotos? [Confirmar] [Cancelar]") e só então chama com `--with-pairs`. As pastas locais nunca são tocadas.

### 5. Notificação com ação

O daemon, ao ver uma conta passar de `ok` para `expired`, cria uma tarefa com `notify-send --wait --action=reauth=Reautorizar ...`. Se a saída do processo for `reauth`, o daemon dispara `omarchy-drive account reauth <conta>` como processo filho. Para evitar repetição, o daemon guarda em memória o conjunto de contas já notificadas, e esse conjunto é limpo quando a conta volta a `ok`.

O daemon disparar o reauth reabre a questão do ambiente da Decisão 1. A mitigação é que o Omarchy inicia a sessão com uwsm, que importa `WAYLAND_DISPLAY` para o systemd do usuário. Se o `xdg-open` falhar, a notificação de erro traz a URL para copiar.

### 6. Atalhos de Docs passam pelo planejador como arquivos cujo conteúdo é do remoto

O provedor do Google passa a:
- pedir `webViewLink` em `FILE_FIELDS`;
- preencher, em itens nativos, `RemoteItem.name` com a extensão do tipo (`Relatório` vira `Relatório.gdoc`, com a tabela de MIME para extensão no provedor);
- preencher `size` e `hash` com o tamanho e o md5 do JSON do atalho, que é determinístico: `{"url","doc_id","mime","account"}` com chaves ordenadas.

Como o hash é o mesmo que a varredura local calcula para o arquivo `.gdoc` em disco, o planejador não precisa de caso especial para rename, move e exclusão: o inode e o id remoto continuam funcionando.

Só três pontos precisam conhecer `native`:
- **Planejador:** se o item remoto é nativo e o local divergiu, o resultado é sempre `Download`. O remoto vence: nunca há `Upload` nem `Conflict`.
- **Motor:** um `Download` de item nativo grava o JSON localmente (arquivo temporário e rename), sem chamada à API.
- **Varredura local:** um arquivo com extensão de atalho que não corresponde a nenhum item remoto nem da base é ignorado, com aviso.

O `remote.retain(!native)` sai.

Para abrir os atalhos, o `install.sh` instala:
- um pacote shared-mime-info (`~/.local/share/mime/packages/omarchy-drive.xml`) que mapeia `*.gdoc` etc. para `application/x-omarchy-drive-shortcut`;
- um `.desktop` com `Exec=omarchy-drive open-url %f`, registrado como padrão via `xdg-mime default`. O subcomando `open-url` lê a `url` do JSON e chama `xdg-open`, aceitando só `https://docs.google.com/` e `https://drive.google.com/`.

- *Alternativa considerada: tratar os nativos fora do planejador, num passo separado.* Foi descartada porque duplicaria a lógica de rename, a lixeira e o limite de exclusão em massa.

### 7. Varredura incremental com snapshot em memória e caminhos sujos

- O `PairEngine` mantém `last_local: HashMap<RelPath, LocalEntry>`.
- O daemon acumula, por par, os caminhos relativos dos eventos relevantes num `HashSet` e os entrega junto com o gatilho do ciclo.
- `sync_once` recebe `ScanScope::Full` ou `ScanScope::Paths(set)`. No modo parcial, para cada caminho sujo:
  - se é um arquivo, faz `stat` e hash (se o tamanho ou o mtime mudaram);
  - se é uma pasta, percorre a subárvore;
  - se `ENOENT`, confirma que o pai existe e é legível antes de remover o caminho e seus descendentes do snapshot. Se o pai também sumiu, sobe até achar um ancestral existente.
- A raiz do par é verificada em todo ciclo, como hoje.
- Um ciclo disparado só pelo polling remoto usa `Paths(vazio)`, ou seja, não lê o disco.
- A varredura completa é forçada em quatro casos:
  - não existe snapshot (partida);
  - o evento tem `need_rescan()` (overflow);
  - houve salto de relógio, quando a diferença entre o relógio de parede e o monotônico desde o último ciclo passa de 60 s (volta da suspensão, sem depender do logind);
  - `last_full` tem mais de 6 h.
- `SETTLE` e `skipped` continuam valendo nos dois modos.
- O teste de equivalência (spec) roda no `MemProvider`: aplica sequências de operações num diretório temporário e compara `plan(full)` com `plan(parcial)`.

### 8. Credenciais: client padrão no momento da compilação e client registrado por conta

- `option_env!("OMARCHY_DRIVE_GOOGLE_CLIENT_ID")`, `..._GOOGLE_CLIENT_SECRET` e `..._MS_CLIENT_ID` são lidas na compilação. O `install.sh` as lê de `packaging/oauth.env`, que fica fora do git (está no `.gitignore`), ou do ambiente.
- `Account` ganha `client_id: Option<String>`.
- A resolução, em `providers::endpoints(cfg, account)`, segue esta ordem:
  1. o `client_id` da conta igual ao de `[oauth]` usa o secret de `[oauth]`;
  2. igual ao padrão embutido usa o secret embutido;
  3. `None` (conta antiga) usa `[oauth]`;
  4. nos demais casos, erro explicando que o client daquela conta não está mais disponível.
- Para uma conta nova, usa `[oauth]` se estiver configurado, e senão o padrão.
- O secret do Google para "app para computador" não é confidencial segundo o próprio Google, então embuti-lo no binário é aceitável. Mesmo assim, ele fica fora do repositório para não ser indexado.

### 9. Páginas públicas para a publicação

Duas páginas estáticas, `site/index.html` e `site/privacy.html`, sem rastreamento e sem scripts de terceiros. A publicação exige um host público. Como o repositório é privado, o GitHub Pages dele depende de plano pago (ver Open Questions). As páginas são cadastradas na tela de branding do Google Auth Platform. O domínio autorizado é o do host escolhido.

### 10. Armazenamento de tokens atrás de uma trait

- `trait TokenStore { load(account), save(account, &TokenSet), delete(account) }`, com duas implementações: `KeyringStore` (crate `oo7`, Secret Service via D-Bus, runtime tokio) e `FileStore` (o comportamento atual).
- O item do keyring tem os atributos `{application: "omarchy-drive", account: <nome>}` e o rótulo "omarchy-drive: <nome>".
- A seleção acontece na partida:
  - Secret Service presente e coleção padrão desbloqueada: `KeyringStore`, com a migração.
  - Presente mas bloqueado: o par fica em `PairState::Offline` com a mensagem "keyring bloqueado". A seleção é refeita a cada polling, sem chamar `unlock()`, para não abrir o prompt em laço.
  - Ausente: `FileStore` com aviso.
- O `TokenManager` guarda o store de onde leu e grava a renovação no mesmo lugar.
- *Alternativa considerada: crate `keyring`.* Foi descartada porque, no Linux, o backend dela é síncrono e puxa `dbus` em C. O `oo7` é Rust puro e assíncrono.

### 11. Validação do OneDrive

- Antes do teste real, entram testes de contrato do provedor com respostas gravadas do Graph (padrão já usado no gdrive):
  - delta com `@odata.deltaLink`;
  - `410 Gone` / `resyncRequired`;
  - upload session;
  - `quickXorHash`.
- O provedor ganha `fn validate_name(&self, rel: &RelPath) -> Result<(), String>` na trait `Provider`, com implementação padrão `Ok`. No OneDrive, a validação recusa:
  - os caracteres `" * : < > ? / \ |`;
  - os nomes reservados (`CON`, `.lock`, `desktop.ini`, `_vti_`);
  - caminhos acima de 400 caracteres.

  O planejador transforma um `Upload` ou `CreateRemoteDir` recusado em aviso no status.
- Para o cursor invalidado, o motor trata `ChangeBatch.reset` com listagem completa. Os itens ausentes na listagem viram "remoto ausente" e passam pelo planejador normalmente: não há exclusão sem base nem exclusão local de itens que continuam no disco.

## Risks / Trade-offs

- **[Risco] O servidor de notificações do Omarchy pode não suportar ações.** → Detectar com `notify-send --action` sem suporte (sai sem `--wait` útil). O fallback é a notificação dizer "abra o painel do Drive Sync para reautorizar". O painel continua sendo o caminho principal.
- **[Risco] O widget perde o `auth.json` se o daemon e o Quickshell reiniciarem durante o fluxo.** → O arquivo é a fonte da verdade e o `pid` é conferido com `kill(pid, 0)`. Um fluxo com processo morto é tratado como `failed`.
- **[Risco] O snapshot em memória diverge do disco** (evento perdido sem overflow, por exemplo em pasta montada via rede). → Varredura completa a cada 6 h e na partida. Pastas de rede ficam documentadas como "apenas varredura completa": opção `full_scan_only` por par.
- **[Risco] Atalho com hash sintético colide com arquivo real `.gdoc` vindo de outra ferramenta.** → Arquivos com extensão de atalho sem item remoto correspondente são ignorados e nunca enviados.
- **[Trade-off] O desconectar remove os pares.** É mais simples para o usuário, mas é destrutivo para a configuração. → Confirmação inline e arquivos locais intactos. O estado SQLite do par é removido; reconectar recria o par com a mesclagem inicial, que mantém as duas cópias em caso de divergência.
- **[Risco] O keyring bloqueado na partida deixa os pares parados até o desbloqueio.** → Estado e mensagem claros no painel. Na sessão normal do Omarchy, o gnome-keyring é desbloqueado pelo PAM no login.
- **[Risco] A publicação no Google pode pedir verificação do escopo `drive` (restrito).** → Sem verificação, o app publicado funciona para até 100 usuários, com aviso de "app não verificado", o que é suficiente para uso pessoal. Se isso mudar, o client próprio continua disponível.

## Migration Plan

1. **Ordem de entrega** (cada item é um commit independente, mantendo `cargo test` e `clippy` limpos):
   1. login gráfico;
   2. credenciais padrão;
   3. atalhos de Docs;
   4. varredura incremental;
   5. keyring;
   6. OneDrive.

   As credenciais padrão vêm logo depois do login gráfico porque o "Conectar" sem setup depende delas.
2. **`status.json` versão 2:** o widget novo lê as versões 1 e 2. Um widget antigo com daemon novo continua funcionando, porque os campos de antes são mantidos.
3. **Config:** os campos novos (`Account.client_id`, `full_scan_only`) são opcionais com `serde(default)`. Uma configuração antiga carrega sem mudança.
4. **Tokens:** a migração para o keyring é automática e idempotente. Para reverter, basta instalar o binário anterior e rodar `account reauth`, já que o arquivo não existe mais.
5. **Atalhos de Docs:** no primeiro ciclo depois da atualização, todos os nativos aparecem como "remoto novo" e geram atalhos. Não há envio ao Drive.

## Open Questions

- **Onde hospedar as páginas públicas:** tornar o repositório público, criar um repositório público só para o site, ou outro host estático. Não muda as páginas nem as tarefas, só a URL cadastrada no Google.
- **Conta Microsoft pessoal ou corporativa para a validação do OneDrive:** a pessoal é suficiente para as specs. A corporativa acrescenta só o teste de consentimento do administrador.

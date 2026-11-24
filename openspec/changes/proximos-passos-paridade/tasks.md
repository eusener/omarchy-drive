# Tasks

## 1. Login gráfico: estado de autenticação no núcleo

- [x] 1.1 Criar `AuthError::Expired` em `core/src/oauth.rs`, retornado quando a renovação responde `invalid_grant`/`interaction_required`; verificar com teste unitário que simula resposta 400 `invalid_grant` e confere o tipo do erro
- [x] 1.2 Adicionar `PairState::AuthExpired` e `StatusDoc.accounts` (`provider`, `label`, `auth: ok|expired`), subir `version` para 2 e incluir `AuthExpired` no cálculo de `overall`; verificar com teste de `status.rs` que serializa e confere o JSON
- [x] 1.3 No motor, converter `AuthError::Expired` em `AuthExpired`; no daemon, parar de ciclar o par nesse estado até um `Reload`; verificar com teste de motor (MemProvider devolvendo `Expired`) e conferindo no log que o par não cicla mais

## 2. Login gráfico: CLI

- [x] 2.1 Criar a função de nome automático (parte local do e-mail normalizada, sufixo `-2`, `-3`...) em `core/src/config.rs`; verificar com testes de nome em uso, acentos e caracteres especiais
- [x] 2.2 Implementar `account connect --provider <p>`: OAuth, depois e-mail, depois reauth se o e-mail já existir no provedor, senão conta nova com nome automático; mandar `Reload` ao final; verificar conectando a conta Google real (deve virar reauth de `pessoal`, sem duplicar)
- [x] 2.3 Gravar o progresso em `$XDG_RUNTIME_DIR/omarchy-drive/auth.json` com `flock`, `pid` e limite de 5 minutos; criar `account cancel`; verificar que um segundo `connect` simultâneo é recusado, que `cancel` encerra o primeiro e que a porta é liberada (`ss -ltn`)
- [x] 2.4 Fazer `account add` e `account reauth` usarem o mesmo caminho e o mesmo `auth.json`; verificar que `account reauth pessoal` continua funcionando
- [x] 2.5 Implementar `account remove --with-pairs`, que remove os pares (configuração e SQLite) e a conta sem tocar nas pastas locais; verificar com par de teste descartável que a pasta local continua no disco
- [x] 2.6 Atualizar o README (seção Uso) com `connect`, `cancel` e `remove --with-pairs`; verificar rodando os comandos exatamente como documentados

## 3. Login gráfico: widget e notificação

- [x] 3.1 Criar no `Panel.qml` a seção "Contas": lista com e-mail, provedor e estado, mais os botões "Conectar Google Drive" e "Conectar OneDrive" que disparam `account connect` com `execDetached`; verificar com captura do painel e a conta aparecendo depois do login
- [x] 3.2 Ler `auth.json` no widget: "Aguardando o navegador…" com "Cancelar", mensagens de falha e de sucesso, e fluxo com processo morto tratado como falha; verificar cancelando pelo painel e matando o processo manualmente
- [x] 3.3 Adicionar "Reautorizar" nas contas `expired` e o estado "Login expirado" nos pares; verificar editando o token para um refresh inválido e conferindo o painel e a volta para "Em dia" depois de reautorizar
- [x] 3.4 Adicionar "Desconectar" com confirmação inline que lista os pares; verificar desconectando uma conta de teste
- [x] 3.5 Mostrar o botão "Sincronizar Meu Drive em ~/GoogleDrive" quando houver conta sem nenhum par; verificar o par criado com os valores padrão
- [x] 3.6 Adicionar as strings novas em `i18n/en.json` e `i18n/pt-BR.json` e manter compatibilidade com `status.json` v1 em `Model.js`; verificar o painel nos dois idiomas e com um `status.json` v1 de exemplo
- [x] 3.7 No daemon, notificar uma única vez quando a conta passa para `expired`, com `--action=reauth=Reautorizar` que dispara `account reauth`, e usar texto de fallback se o servidor de notificações não suportar ações; verificar a notificação, o clique e a ausência de repetição nos ciclos seguintes

## 4. Credenciais OAuth sem setup manual

- [x] 4.1 Ler `OMARCHY_DRIVE_GOOGLE_CLIENT_ID`, `..._GOOGLE_CLIENT_SECRET` e `..._MS_CLIENT_ID` com `option_env!`; o `install.sh` carrega `packaging/oauth.env` (adicionado ao `.gitignore`); verificar que `git status` não mostra o arquivo e que `strings` encontra o id no binário instalado
- [x] 4.2 Adicionar `Account.client_id` e a resolução por conta em `providers::endpoints` (`[oauth]`, depois o padrão, depois conta antiga, depois erro), gravando o client usado no connect; verificar com testes unitários dos quatro casos e com a conta `pessoal` (sem `client_id`) continuando a sincronizar
- [x] 4.3 Sem client padrão nem configurado, `connect` deve falhar com mensagem apontando o README, sem abrir o navegador; verificar compilando sem `oauth.env`
- [x] 4.4 Criar `docs/index.html` e `docs/privacy.html` (o GitHub Pages só serve a raiz ou `/docs`) (estáticas, sem scripts de terceiros) e publicá-las no host escolhido; verificar com `curl -I` nas duas URLs públicas
- [x] 4.5 Guiar o usuário na tela de branding (página inicial, política, domínio autorizado) e na publicação "Em produção"; verificar o status "Em produção" no console e atualizar o README (seção Credenciais) dizendo que o vencimento de 7 dias não se aplica mais

## 5. Atalhos de Google Docs

- [x] 5.1 No provedor gdrive (URL de edição derivada do id, sem `webViewLink`, para o atalho ser determinístico), criar a tabela de MIME para extensão e preencher nome com extensão, tamanho e md5 do JSON determinístico; verificar com testes de `map_file` para Doc, Planilha, Apresentação e tipo desconhecido (`.glink`)
- [x] 5.2 No planejador, fazer o remoto sempre vencer para itens nativos (divergência local vira `Download`, nunca `Upload`/`Conflict`); verificar com testes do planejador para edição local, edição dupla, rename local e exclusão local de atalho
- [x] 5.3 No motor, baixar nativos pelo caminho normal (temporário, hash e rename), com o provedor gerando o JSON localmente sem rede, e remover o `retain(!native)`; verificar com teste ponta a ponta no MemProvider que nenhum `download()` do provedor é chamado
- [x] 5.4 No planejador, ignorar com aviso arquivos de atalho sem item remoto nem base; verificar com teste de `.gdoc` copiado de fora
- [x] 5.5 Implementar `omarchy-drive open-url <arquivo>` (só `https://docs.google.com/` e `https://drive.google.com/`); o `install.sh` instala o MIME XML e o `.desktop` e roda `update-mime-database` e `xdg-mime default`; verificar `xdg-mime query filetype` num `.gdoc` e a abertura pelo Nautilus
- [x] 5.6 Validar contra o Drive real: criar um Doc na pasta de teste, conferir o atalho local, renomear e apagar localmente, e conferir no Drive (rename e lixeira); documentar no README e mudar a linha do roadmap para ✅

## 6. Varredura local incremental

- [x] 6.1 Criar `ScanScope::{Full, Paths}` e a varredura parcial em `scan.rs` (stat e hash do arquivo, subárvore de pasta, `ENOENT` confirmado pelo pai), com snapshot `last_local` no `PairEngine`; verificar com testes unitários de criação, edição, rename por editor (remove e recria), pasta movida para dentro e pai apagado
- [x] 6.2 Implementar o teste de equivalência: sequências de operações num diretório temporário comparando `plan(full)` com `plan(parcial)`; verificar `cargo test` passando com pelo menos 20 sequências
- [x] 6.3 No daemon, acumular os caminhos sujos por par e forçar varredura completa na partida, em `need_rescan()`, em salto de relógio acima de 60 s e a cada 6 h; ciclo de polling sem eventos não lê o disco; verificar no log (`scope=paths n=1` numa edição, `scope=full` na partida)
- [x] 6.4 Adicionar a opção `full_scan_only` por par (pastas de rede) e documentar no README; verificar que um par com a opção sempre registra `scope=full`
- [x] 6.5 Medir antes e depois numa árvore de cerca de 50 mil arquivos (benchmark da varredura completa contra a parcial; um par real com 50 mil arquivos exigiria 50 mil uploads ao Drive) e registrar no README; atualizar a linha do roadmap

## 7. Tokens no keyring

- [x] 7.1 Criar a trait `TokenStore` com `FileStore` (comportamento atual) e trocar o `TokenManager` para usá-la; verificar que todos os testes existentes passam sem mudança
- [x] 7.2 Implementar `KeyringStore` com `oo7` (atributos `application`/`account`); verificar gravando, lendo e apagando um item de teste e conferindo com `secret-tool search application omarchy-drive`
- [x] 7.3 Implementar a seleção na partida (desbloqueado, bloqueado sem chamar `unlock()`, ausente com aviso único) e a migração idempotente que só apaga o arquivo depois de reler o item; verificar migrando `pessoal.json`, conferindo que o arquivo sumiu e que a sincronização continua
- [x] 7.4 Fazer a renovação gravar no mesmo store e o `remove` apagar item e arquivo; verificar forçando uma renovação (`expires_at` no passado) e removendo uma conta de teste
- [x] 7.5 Atualizar o README (onde ficam os tokens, fallback, keyring bloqueado) e mudar a linha do roadmap para ✅; verificar a seção relida contra o comportamento

## 8. OneDrive validado

- [x] 8.1 Criar testes de contrato do provedor com respostas gravadas do Graph: delta com `deltaLink`, `410`/`resyncRequired`, upload session e `quickXorHash`; verificar `cargo test -p omarchy-drive-onedrive`
- [x] 8.2 Adicionar `Provider::validate_name` (padrão `Ok`) com as regras do OneDrive; o planejador transforma uploads recusados em aviso; verificar com testes de `relatório: final.txt`, `CON` e caminho longo
- [x] 8.3 Confirmar que `ChangeBatch.reset` faz listagem completa sem exclusões falsas; verificar com teste de motor que simula reset com a árvore inalterada e confere zero ações
- [ ] 8.4 Guiar o usuário no cadastro do app no Microsoft Entra e rodar `setup microsoft` (ou usar o client padrão do grupo 4); verificar com `account connect --provider onedrive` concluído
- [ ] 8.5 Rodar a bateria real num par de teste (upload de 20 MB em partes, download, rename remoto sem retransferir, edição local, conflito, exclusão para a lixeira, ciclo ocioso com zero ações) e corrigir os defeitos com teste de regressão; verificar cada item pela API do Graph e mudar a linha do roadmap para ✅

## 9. Integração final

- [ ] 9.1 Do zero, sem terminal: instalar, conectar o Google pelo widget, criar o par padrão, abrir um `.gdoc`, desconectar; verificar com capturas de cada etapa
- [ ] 9.2 Rodar `cargo test --workspace` e `cargo clippy --workspace --all-targets` limpos, com o daemon ativo por 24 h sem erros no `journalctl`; verificar a saída e o log

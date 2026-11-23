# Proposal

## Why

O omarchy-drive já sincroniza um par de teste com o Google Drive real, mas ainda não substitui um app pago no dia a dia:
- conectar ou renovar uma conta exige terminal;
- o login vence a cada 7 dias;
- Google Docs somem da pasta local;
- cada ciclo varre a pasta inteira;
- o OneDrive nunca foi testado contra a API real;
- tokens ficam em arquivo.

Este change reúne os seis próximos passos que fecham essa distância. O login gráfico sem linha de comando entra como prioridade, porque é a primeira coisa que um usuário não técnico encontra.

## What Changes

1. **Login gráfico, sem linha de comando.** O painel do widget na barra ganha uma seção "Contas" com estas ações:
   - "Conectar Google Drive" e "Conectar OneDrive", que abrem o navegador para o OAuth. O nome da conta é gerado a partir do e-mail.
   - "Reautorizar", que aparece quando o token venceu ou foi revogado.
   - "Desconectar".

   O daemon passa a expor contas e o estado de autenticação no `status.json`. A notificação de token vencido ganha a ação "Reautorizar". O fluxo inteiro roda sem abrir terminal: o widget dispara a CLI em segundo plano e acompanha o progresso por um arquivo de estado.
2. **Google Docs, Planilhas e Apresentações como atalhos.** Cada arquivo nativo do Google vira um arquivo local `.gdoc`, `.gsheet` ou `.gslides`, só de leitura. Abrir esse arquivo abre o documento no navegador. Hoje esses arquivos são ignorados.
3. **Varredura local incremental.** Os eventos do inotify marcam quais caminhos mudaram, e o ciclo varre só esses caminhos. A varredura completa continua como reconciliação periódica e roda na partida, depois de overflow do inotify e depois de suspender o computador.
4. **Credenciais OAuth sem setup manual.** O binário passa a trazer um client OAuth padrão, configurável na compilação. Com isso, conectar uma conta não depende mais de `omarchy-drive setup`. Também entram uma página de política de privacidade e os passos para publicar o app no Google, o que tira o vencimento de 7 dias. `setup google|microsoft` continua existindo para quem quiser usar o próprio client.
5. **OneDrive validado contra a API real.** Repetir no Microsoft Graph a mesma bateria de testes feita no Google Drive:
   - upload em partes;
   - rename sem retransferir;
   - conflito;
   - exclusão para a lixeira;
   - ciclo ocioso.

   Os defeitos encontrados serão corrigidos, com testes de regressão para o provedor.
6. **Tokens no keyring do sistema.** Os tokens passam para o Secret Service (gnome-keyring ou KWallet). Na primeira execução, os arquivos `tokens/*.json` existentes são migrados e apagados. Sem keyring disponível, o arquivo 0600 continua sendo usado como fallback, e o usuário é avisado.

**Fora do escopo:** commit e push do código pendente (tarefa operacional), limite de banda, emblemas no Nautilus, histórico de versões e arquivos sob demanda via FUSE (fase 2).

## Capabilities

### New Capabilities
- `account-login-ui`: conectar, reautorizar e desconectar contas pelo widget da barra e por notificações, sem terminal, incluindo como o estado de autenticação é exposto.
- `gdrive-native-docs`: representação local dos arquivos nativos do Google (Docs, Planilhas, Apresentações etc.) como atalhos só de leitura.
- `incremental-local-scan`: varredura local guiada pelo inotify, com reconciliação completa periódica e garantias de que nada é perdido.
- `oauth-client-config`: de onde vêm as credenciais OAuth (client padrão embutido ou client próprio) e a publicação do app.
- `onedrive-sync`: comportamento exigido do provedor OneDrive (delta, upload em partes, QuickXorHash, lixeira, renames), validado contra a API real.
- `secure-token-storage`: armazenamento de tokens no keyring do sistema, com migração e fallback.

### Modified Capabilities
(nenhuma; o projeto ainda não tem specs publicadas)

## Impact

- **`crates/core`:**
  - `oauth.rs`: armazenamento de tokens atrás de uma trait, com keyring e arquivo como implementações;
  - `status.rs`: contas e estado de autenticação no `status.json`;
  - `scan.rs` e `engine.rs`: varredura incremental;
  - `config.rs`: client padrão.
- **`crates/app`:**
  - `cli.rs`: novos comandos `account connect` (sem nome), `account cancel`, `account remove --with-pairs` e `open-url`;
  - `daemon.rs`: estado de autenticação por conta, notificação com ação, caminhos sujos do inotify entregues ao motor;
  - `ipc.rs`: sem requisições novas (o `Reload` já cobre a troca de tokens).
- **`crates/provider-gdrive`:** URL `webViewLink` dos arquivos nativos e criação dos atalhos.
- **`crates/provider-onedrive`:** correções que a validação real revelar.
- **`shell-plugin/`:** seção "Contas" no painel, leitura de `auth.json`, novos estados, strings novas em `i18n/en.json` e `i18n/pt-BR.json`.
- **`install.sh` e `packaging/`:** tipo MIME e `.desktop` dos atalhos, e `oauth.env` (fora do git) com o client padrão.
- **Dependências novas:** `oo7` (Secret Service, Rust puro), para o keyring.
- **Fora do código:** páginas inicial e de política de privacidade num host público, cadastro do app no Microsoft Entra e publicação do app no Google Cloud. Tudo isso depende de ações do usuário nos consoles.

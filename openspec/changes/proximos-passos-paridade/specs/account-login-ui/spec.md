# Spec Delta

## Purpose

Permite conectar, reautorizar e desconectar contas do Google Drive e do OneDrive pela interface gráfica do Omarchy (widget da barra e notificações), sem abrir um terminal.

## ADDED Requirements

### Requirement: Conectar conta pelo widget
O painel do widget SHALL ter uma seção "Contas" com as ações "Conectar Google Drive" e "Conectar OneDrive". Cada ação MUST iniciar o fluxo OAuth no navegador padrão, sem exigir terminal nem a digitação de nome de conta.

#### Scenario: Conexão bem-sucedida
- **WHEN** o usuário clica em "Conectar Google Drive" e conclui o login no navegador
- **THEN** a conta aparece na seção "Contas" com o e-mail e o provedor, e o usuário recebe uma notificação de sucesso

#### Scenario: Login cancelado ou negado
- **WHEN** o usuário nega o consentimento ou fecha a aba sem concluir
- **THEN** nenhuma conta é criada, nenhum token é gravado e o painel mostra que a conexão não foi concluída

### Requirement: Nome da conta gerado automaticamente
Uma conta conectada pela interface gráfica SHALL receber um nome único derivado do e-mail (a parte antes do `@`, normalizada para minúsculas, letras, dígitos e hífen). O nome MUST receber um sufixo numérico se já existir outra conta com ele.

#### Scenario: Nome em uso
- **WHEN** já existe uma conta `joao` e o usuário conecta outra conta cujo e-mail começa com `joao@`
- **THEN** a nova conta é criada com o nome `joao-2`

#### Scenario: Mesma conta conectada duas vezes
- **WHEN** o usuário conecta uma conta do provedor cujo e-mail já pertence a uma conta configurada do mesmo provedor
- **THEN** nenhuma conta duplicada é criada, os tokens da conta existente são atualizados e o painel informa que ela foi reautorizada

### Requirement: Apenas um fluxo de autorização por vez
O sistema SHALL permitir no máximo um fluxo OAuth em andamento por vez. O fluxo MUST expirar depois de 5 minutos sem resposta e MUST poder ser cancelado pelo painel.

#### Scenario: Segundo clique durante um fluxo
- **WHEN** há um fluxo em andamento e o usuário clica de novo em "Conectar"
- **THEN** nenhum novo fluxo é iniciado e o painel mostra "Aguardando o navegador…" com a opção "Cancelar"

#### Scenario: Tempo esgotado
- **WHEN** passam 5 minutos sem que o navegador devolva a autorização
- **THEN** o fluxo é encerrado, a porta local é liberada e o painel volta ao estado normal

### Requirement: Estado de autenticação visível
O sistema SHALL expor para o widget, por conta, o estado de autenticação: `ok`, `expired` ou `authorizing`. Uma conta cujo token foi recusado pelo provedor MUST aparecer como `expired`, e seus pares MUST aparecer com o estado "Login expirado" em vez de um erro genérico.

#### Scenario: Refresh token vencido
- **WHEN** o provedor recusa a renovação do token de uma conta
- **THEN** a conta aparece como `expired`, os pares dessa conta param de tentar sincronizar e o ícone da barra indica atenção

### Requirement: Reautorizar pela interface
Uma conta com estado `expired` SHALL ter a ação "Reautorizar" no painel. A reautorização MUST manter pares, estado de sincronização e nome da conta, e MUST recusar um login com um e-mail diferente do da conta.

#### Scenario: Reautorização com a mesma conta
- **WHEN** o usuário clica em "Reautorizar" e entra com o mesmo e-mail
- **THEN** os tokens são substituídos e os pares da conta voltam a sincronizar sem reiniciar o daemon

#### Scenario: Login com outra conta
- **WHEN** o usuário reautoriza mas entra no navegador com outro e-mail
- **THEN** os tokens anteriores são mantidos, nada é alterado e o painel explica que o login foi feito com outra conta

### Requirement: Notificação de login expirado com ação
Quando uma conta passar para `expired`, o sistema SHALL mostrar uma única notificação de desktop com a ação "Reautorizar", que inicia o mesmo fluxo do painel.

#### Scenario: Clique na ação da notificação
- **WHEN** o usuário clica em "Reautorizar" na notificação
- **THEN** o navegador abre no login do provedor da conta

#### Scenario: Sem repetição
- **WHEN** a conta continua `expired` nos ciclos seguintes
- **THEN** nenhuma nova notificação é mostrada até a conta voltar a `ok` e expirar de novo

### Requirement: Desconectar conta pela interface
O painel SHALL oferecer "Desconectar" em cada conta. Se a conta tiver pares, o painel MUST pedir confirmação listando esses pares. A desconexão MUST remover a conta, os seus pares e os tokens, e MUST NOT apagar arquivos locais nem remotos.

#### Scenario: Desconectar conta com pares
- **WHEN** o usuário clica em "Desconectar" numa conta com o par `drive` e confirma
- **THEN** a conta, o par e os tokens são removidos, e a pasta local do par continua intacta no disco

### Requirement: Paridade com a CLI
Os comandos `account connect`, `account reauth` e `account remove --with-pairs` SHALL produzir o mesmo resultado que as ações correspondentes da interface. Uma mudança feita por um dos caminhos MUST aparecer no outro sem reiniciar o daemon.

#### Scenario: Conta criada pela CLI
- **WHEN** o usuário roda `omarchy-drive account add pessoal --provider gdrive` com o daemon ativo
- **THEN** a conta aparece na seção "Contas" do painel em até 5 segundos

# Spec Delta

## Purpose

Define de onde vêm as credenciais OAuth usadas para conectar contas, para que um usuário consiga conectar o Google Drive ou o OneDrive sem cadastrar um app nos consoles dos provedores, e para que o login não vença a cada 7 dias.

## ADDED Requirements

### Requirement: Client OAuth padrão embutido
O binário SHALL poder ser compilado com um client OAuth padrão por provedor (id e, no Google, o secret de app instalado). Com o client padrão presente, conectar uma conta MUST funcionar sem rodar `omarchy-drive setup`.

#### Scenario: Instalação nova com client padrão
- **WHEN** o binário foi compilado com client padrão e o usuário clica em "Conectar Google Drive" sem nunca ter rodado `setup`
- **THEN** o fluxo OAuth abre normalmente com o client padrão

#### Scenario: Compilação sem client padrão
- **WHEN** o binário foi compilado sem client padrão e não há client configurado
- **THEN** a ação de conectar explica que é preciso configurar um client e aponta o passo a passo do README, sem abrir o navegador

### Requirement: Client próprio tem prioridade
Um client configurado pelo usuário com `omarchy-drive setup google|microsoft` SHALL ter prioridade sobre o client padrão. Uma conta MUST continuar usando o client com o qual foi autorizada.

#### Scenario: Usuário configura client próprio depois
- **WHEN** existe uma conta autorizada com o client padrão e o usuário roda `setup google` com outro client
- **THEN** a conta existente continua renovando o token com o client original, e só as contas conectadas depois usam o novo client

### Requirement: Client registrado por conta
O sistema SHALL guardar, junto de cada conta, qual client a autorizou (padrão ou próprio, com o id do client). Os segredos MUST NOT ser duplicados no arquivo de configuração.

#### Scenario: Conta antiga sem registro
- **WHEN** uma conta criada antes desta mudança não tem client registrado
- **THEN** ela é tratada como autorizada pelo client configurado em `[oauth]`, como hoje

### Requirement: Página de política de privacidade
O projeto SHALL publicar uma página de política de privacidade e uma página inicial com URLs públicas estáveis. As páginas MUST declarar que os dados do Drive só trafegam entre o computador do usuário e o provedor, sem servidor intermediário.

#### Scenario: Pré-requisito da publicação no Google
- **WHEN** o app OAuth é enviado para o status "Em produção" no Google Cloud
- **THEN** as URLs da página inicial e da política de privacidade estão acessíveis publicamente e são as que estão cadastradas na tela de branding

### Requirement: Sem vencimento semanal após a publicação
Com o app publicado ("Em produção"), uma conta Google SHALL continuar sincronizando por mais de 7 dias sem reautorização, enquanto o usuário não revogar o acesso.

#### Scenario: Oito dias depois
- **WHEN** uma conta foi autorizada com o app publicado há 8 dias
- **THEN** a renovação do token funciona e a conta continua com estado `ok`

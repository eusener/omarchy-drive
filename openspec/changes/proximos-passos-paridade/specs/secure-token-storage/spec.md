# Spec Delta

## Purpose

Guarda os tokens OAuth das contas no keyring do sistema (Secret Service), e não mais em arquivos no disco, mantendo um fallback seguro onde não houver keyring disponível.

## ADDED Requirements

### Requirement: Tokens no keyring
Quando houver um serviço Secret Service disponível na sessão, o sistema SHALL gravar e ler os tokens de cada conta no keyring, com um item por conta identificado pelo aplicativo e pelo nome da conta.

#### Scenario: Conta nova com keyring disponível
- **WHEN** o usuário conecta uma conta e o gnome-keyring está ativo
- **THEN** os tokens ficam num item do keyring e nenhum arquivo de token é criado no disco

### Requirement: Migração dos arquivos existentes
Na primeira execução com keyring disponível, o sistema SHALL mover para o keyring os tokens de cada arquivo `tokens/<conta>.json` existente. O arquivo MUST ser apagado só depois de o item ser gravado e relido com sucesso.

#### Scenario: Migração bem-sucedida
- **WHEN** existe `tokens/pessoal.json` e o daemon inicia com o keyring disponível
- **THEN** o token passa para o keyring, o arquivo é apagado e a conta continua sincronizando sem reautorização

#### Scenario: Falha ao gravar no keyring
- **WHEN** a gravação no keyring falha durante a migração
- **THEN** o arquivo é mantido, a conta continua usando o arquivo e a migração é tentada de novo na próxima partida

### Requirement: Fallback para arquivo
Sem keyring disponível, o sistema SHALL continuar usando arquivos de token com permissão 0600 numa pasta 0700, e MUST avisar o usuário uma vez (no log e no status) que os tokens estão em arquivo.

#### Scenario: Sessão sem Secret Service
- **WHEN** o daemon inicia numa sessão sem nenhum serviço Secret Service
- **THEN** a sincronização funciona com os arquivos de token, e o status mostra um aviso único sobre o armazenamento em arquivo

### Requirement: Keyring bloqueado
Se o keyring existir mas estiver bloqueado, o sistema MUST NOT recorrer a arquivo nem pedir senha em loop. Os pares das contas afetadas SHALL esperar o desbloqueio e retomar sozinhos.

#### Scenario: Keyring desbloqueado depois do login
- **WHEN** o daemon inicia antes do desbloqueio do keyring e o usuário desbloqueia depois
- **THEN** os pares ficam aguardando sem erro repetido e começam a sincronizar após o desbloqueio

### Requirement: Remoção apaga o segredo
Desconectar uma conta SHALL apagar o item correspondente do keyring e qualquer arquivo de token restante da conta.

#### Scenario: Desconectar conta
- **WHEN** o usuário desconecta a conta `pessoal`
- **THEN** não resta nenhum item do keyring nem arquivo de token para `pessoal`

### Requirement: Renovação persistida no mesmo lugar
Quando o access token for renovado, o sistema SHALL gravar o token novo no mesmo armazenamento de onde o leu, e MUST NOT criar cópia em outro lugar.

#### Scenario: Renovação com keyring
- **WHEN** o access token de uma conta guardada no keyring expira e é renovado
- **THEN** o item do keyring é atualizado e nenhum arquivo de token é criado

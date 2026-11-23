# Spec Delta

## Purpose

Define o comportamento exigido do provedor OneDrive (contas pessoais e corporativas da Microsoft) para que ele entregue as mesmas garantias de sincronização já validadas com o Google Drive.

## ADDED Requirements

### Requirement: Mudanças remotas incrementais
O provedor SHALL obter as mudanças remotas a partir do último cursor salvo, sem relistar a árvore. Se o OneDrive invalidar o cursor, o provedor MUST fazer uma listagem completa e retomar a partir de um cursor novo, sem gerar exclusões falsas.

#### Scenario: Cursor expirado
- **WHEN** o OneDrive responde que o token de delta não é mais válido
- **THEN** o ciclo faz uma listagem completa, o plano não contém exclusões de itens que continuam existindo e um cursor novo é salvo

### Requirement: Integridade das transferências
Todo download SHALL ser conferido pelo QuickXorHash informado pelo OneDrive antes de substituir o arquivo local. Todo upload MUST ter o hash conferido depois de concluído.

#### Scenario: Upload grande em partes
- **WHEN** um arquivo de 20 MB é criado localmente
- **THEN** ele é enviado em partes, e o QuickXorHash remoto é igual ao calculado localmente

#### Scenario: Hash divergente no download
- **WHEN** o conteúdo baixado não confere com o hash informado
- **THEN** o arquivo temporário é descartado, o arquivo local não é alterado e o item é tentado de novo no ciclo seguinte

### Requirement: Rename e move sem retransferência
Renames e moves feitos de qualquer um dos lados SHALL ser aplicados do outro lado como rename ou move, sem transferir o conteúdo de novo.

#### Scenario: Rename no OneDrive
- **WHEN** um arquivo é renomeado pela interface web do OneDrive
- **THEN** o arquivo local é renomeado e nenhum download acontece

### Requirement: Exclusão remota vai para a lixeira
Exclusões propagadas para o OneDrive SHALL mover o item para a lixeira do OneDrive e MUST NOT apagá-lo definitivamente.

#### Scenario: Arquivo apagado localmente
- **WHEN** um arquivo sincronizado é apagado da pasta local
- **THEN** o item aparece na lixeira do OneDrive e pode ser restaurado de lá

### Requirement: Conflito mantém as duas versões
Quando o mesmo arquivo for editado nos dois lados entre dois ciclos, o sistema SHALL manter as duas versões nos dois lados, com o mesmo padrão de nome usado no Google Drive.

#### Scenario: Edição simultânea
- **WHEN** um arquivo é editado no OneDrive e localmente antes do ciclo seguinte
- **THEN** existem `arquivo.ext` e `arquivo (conflito <host> <data>).ext` nos dois lados

### Requirement: Ciclo ocioso sem ações
Depois de uma sincronização completa, sem mudanças em nenhum dos lados, o ciclo SHALL terminar sem executar nenhuma ação e sem transferir dados.

#### Scenario: Ciclo seguinte sem mudanças
- **WHEN** um ciclo roda logo depois de outro que terminou "em dia", sem mudanças em nenhum lado
- **THEN** o relatório do ciclo tem zero ações executadas

### Requirement: Nomes inválidos no OneDrive
Arquivos locais cujo nome o OneDrive não aceita (caracteres reservados, nomes reservados ou caminho longo demais) MUST NOT interromper o ciclo. O sistema SHALL ignorá-los e mostrar um aviso no status do par.

#### Scenario: Arquivo com caractere reservado
- **WHEN** existe localmente um arquivo chamado `relatório: final.txt`
- **THEN** os demais arquivos sincronizam normalmente e o status do par mostra um aviso nomeando o arquivo não enviado

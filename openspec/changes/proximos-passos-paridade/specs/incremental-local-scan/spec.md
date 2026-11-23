# Spec Delta

## Purpose

Reduz o custo de disco e de CPU de cada ciclo de sincronização: a pasta local é varrida só nos caminhos que mudaram, sem abrir mão da garantia de que nenhuma mudança local é perdida ou interpretada errado.

## ADDED Requirements

### Requirement: Ciclo disparado por evento varre só o que mudou
Quando um ciclo for disparado por eventos do sistema de arquivos, o sistema SHALL reler do disco apenas os caminhos afetados pelos eventos e seus descendentes. O restante MUST vir do último estado conhecido.

#### Scenario: Edição de um arquivo numa pasta grande
- **WHEN** um par tem 100 mil arquivos e o usuário edita um deles
- **THEN** o ciclo seguinte lê os metadados só desse arquivo (e da pasta pai) e sincroniza a edição

#### Scenario: Pasta movida para dentro do par
- **WHEN** uma pasta com subpastas é movida de fora para dentro da pasta do par
- **THEN** a pasta e todos os seus descendentes são varridos e enviados

### Requirement: Reconciliação completa periódica
O sistema SHALL fazer uma varredura completa da pasta local em quatro situações:
- na partida do daemon;
- quando a fila de eventos do sistema de arquivos estourar;
- quando o computador voltar da suspensão;
- pelo menos a cada 6 horas.

#### Scenario: Estouro da fila de eventos
- **WHEN** o kernel avisa que eventos foram perdidos
- **THEN** o ciclo seguinte faz uma varredura completa

#### Scenario: Mudança feita com o daemon parado
- **WHEN** um arquivo é criado na pasta do par com o daemon parado e o daemon é iniciado depois
- **THEN** o primeiro ciclo detecta e envia o arquivo

### Requirement: Garantias de segurança preservadas
A varredura incremental MUST manter todas as garantias da varredura completa:
- caminho que não pôde ser lido nunca é tratado como apagado;
- arquivo ainda sendo escrito fica fora do ciclo;
- pasta local ausente aborta o ciclo.

Um caminho só SHALL ser considerado apagado se foi confirmado como ausente no disco durante o ciclo.

#### Scenario: Evento de exclusão de caminho que ainda existe
- **WHEN** chega um evento de remoção, mas o arquivo existe no disco no momento do ciclo (por exemplo, um editor que salva por rename)
- **THEN** o arquivo é tratado como existente e nada é apagado no remoto

#### Scenario: Pasta do par desmontada
- **WHEN** a pasta local do par deixa de existir entre dois ciclos incrementais
- **THEN** o ciclo é abortado e nenhuma exclusão remota acontece

### Requirement: Mesmo resultado que a varredura completa
Para qualquer sequência de mudanças locais capturada pelos eventos, o plano produzido a partir da varredura incremental SHALL ser igual ao plano produzido por uma varredura completa no mesmo momento.

#### Scenario: Verificação por teste
- **WHEN** uma sequência de criações, edições, renames e exclusões é aplicada e os dois modos de varredura rodam
- **THEN** os dois planos contêm as mesmas ações

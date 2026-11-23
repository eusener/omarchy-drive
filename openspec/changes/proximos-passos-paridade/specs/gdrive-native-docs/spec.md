# Spec Delta

## Purpose

Garante que Docs, Planilhas, Apresentações e outros arquivos nativos do Google, que não têm conteúdo binário para baixar, apareçam na pasta local como atalhos que abrem o documento no navegador.

## ADDED Requirements

### Requirement: Atalho local para arquivo nativo
Para cada arquivo nativo do Google dentro de um par, o sistema SHALL criar na pasta local correspondente um arquivo de atalho. O atalho MUST ter o nome do documento com a extensão do tipo: `.gdoc`, `.gsheet`, `.gslides`, `.gdraw`, `.gform` ou `.gmap`, e `.glink` para os demais tipos.

#### Scenario: Documento remoto novo
- **WHEN** um Google Doc chamado `Relatório` é criado na pasta remota de um par
- **THEN** o arquivo `Relatório.gdoc` aparece na pasta local correspondente no ciclo seguinte

### Requirement: Conteúdo do atalho
O atalho SHALL ser um JSON pequeno com, no mínimo, a URL do documento, o id remoto e o tipo, e MUST abrir o documento no navegador padrão quando aberto pelo gerenciador de arquivos.

#### Scenario: Abrir o atalho
- **WHEN** o usuário dá dois cliques em `Relatório.gdoc` no gerenciador de arquivos
- **THEN** o documento abre no navegador na URL de edição do Google

### Requirement: Atalhos são só de leitura para a sincronização
Mudanças locais no conteúdo de um atalho MUST NOT ser enviadas ao Drive. Se o atalho for editado localmente, o sistema SHALL restaurar o conteúdo correto no ciclo seguinte.

#### Scenario: Atalho editado localmente
- **WHEN** o usuário altera o conteúdo de `Relatório.gdoc`
- **THEN** nada é enviado ao Drive e o atalho é regravado com o conteúdo original

### Requirement: Rename e exclusão de atalhos seguem o documento
Renomear, mover ou apagar um atalho localmente SHALL renomear, mover ou mandar para a lixeira o documento remoto correspondente, com as mesmas proteções dos demais arquivos (lixeira e limite de exclusão em massa). Renomear ou apagar o documento remoto SHALL refletir no atalho local.

#### Scenario: Rename local do atalho
- **WHEN** o usuário renomeia `Relatório.gdoc` para `Relatório final.gdoc`
- **THEN** o documento remoto passa a se chamar `Relatório final`

#### Scenario: Exclusão local do atalho
- **WHEN** o usuário apaga `Relatório.gdoc`
- **THEN** o documento vai para a lixeira do Drive e não é apagado definitivamente

### Requirement: Atalho criado localmente não vira documento
Um arquivo `.gdoc`, `.gsheet` ou similar criado localmente que não corresponde a nenhum documento remoto conhecido MUST NOT ser enviado como upload. O sistema SHALL ignorá-lo e registrar um aviso.

#### Scenario: Atalho copiado de outra máquina
- **WHEN** o usuário copia para a pasta do par um `.gdoc` cujo id não pertence ao par
- **THEN** nenhum upload acontece e o status do par mostra um aviso sobre o arquivo ignorado

### Requirement: Atalhos não contam como transferência de dados
Criar ou atualizar atalhos MUST NOT baixar o conteúdo do documento nem exportá-lo. O espaço ocupado no disco SHALL se limitar ao tamanho do JSON do atalho.

#### Scenario: Planilha grande
- **WHEN** a pasta remota contém uma Planilha Google com 50 MB de dados
- **THEN** o arquivo local `.gsheet` tem menos de 1 KB e nenhum dado da planilha é baixado

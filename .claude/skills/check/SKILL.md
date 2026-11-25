---
name: check
description: Verificação completa do omarchy-drive antes de commit ou de dar uma tarefa por concluída — cargo fmt --check, clippy com -D warnings, testes do workspace e qmllint no widget da barra. Use ao terminar mudanças em Rust ou QML, antes de commitar, ou quando o usuário pedir /check.
---

# /check

Roda as verificações do projeto, na ordem, a partir da raiz do repositório. Continue mesmo
quando um passo falhar, para entregar o quadro completo de uma vez.

## Passos

1. **Formatação**
   ```bash
   cargo fmt --all --check
   ```
   Se falhar, rode `cargo fmt --all` e conte quais arquivos mudaram.

2. **Clippy**
   ```bash
   cargo clippy --workspace --all-targets -- -D warnings
   ```

3. **Testes**
   ```bash
   cargo test --workspace
   ```
   Os benchmarks (`bench_*`) são `#[ignore]` e ficam de fora; só rode-os se o usuário pedir:
   `cargo test -p omarchy-drive-core --release bench_ -- --ignored --nocapture`.

4. **Widget QML**
   ```bash
   /usr/lib/qt6/bin/qmllint --unqualified disable shell-plugin/*.qml
   ```
   O módulo do Quickshell/Omarchy não é resolvido fora da barra, então avisos de acesso não
   qualificado são ruído e ficam desligados. Considere só erros e avisos de outros tipos.
   Valide também o manifesto: `jq empty shell-plugin/manifest.json shell-plugin/i18n/*.json`.

## Relatório

Termine com uma tabela curta, um passo por linha (✅/❌), e para cada falha o arquivo:linha e a
mensagem essencial. Não diga que está tudo certo se algum passo falhou ou não rodou.

Se o argumento `fix` for passado (`/check fix`), corrija o que falhou (fmt, avisos do clippy,
testes quebrados pela mudança atual) e rode a verificação de novo. Sem `fix`, só relate.

Nunca mexa em `packaging/oauth.env` nem edite `Cargo.lock` à mão para fazer algo passar.

#!/usr/bin/env bash
# PreToolUse (Edit|Write|NotebookEdit): impede edição direta de arquivos sensíveis.
#  - packaging/oauth.env: client OAuth real, fora do git.
#  - Cargo.lock: só deve mudar via cargo.
f=$(jq -r '.tool_input.file_path // .tool_input.notebook_path // empty')
[ -n "$f" ] || exit 0
case "$f" in
  */packaging/oauth.env | packaging/oauth.env)
    echo "Bloqueado: packaging/oauth.env guarda o client OAuth real e não deve ser editado pelo Claude. Peça ao usuário para editar manualmente." >&2
    exit 2 ;;
  */Cargo.lock | Cargo.lock)
    echo "Bloqueado: Cargo.lock não é editado à mão. Use cargo (cargo update -p <crate>, cargo add, cargo build)." >&2
    exit 2 ;;
esac
exit 0

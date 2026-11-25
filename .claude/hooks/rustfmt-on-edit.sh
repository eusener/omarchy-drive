#!/usr/bin/env bash
# PostToolUse (Edit|Write): formata com rustfmt o arquivo .rs que acabou de ser editado.
# Nunca bloqueia: se o rustfmt falhar (ex.: erro de sintaxe), só avisa.
f=$(jq -r '.tool_input.file_path // empty')
[[ "$f" == *.rs && -f "$f" ]] || exit 0
if ! out=$(rustfmt --edition 2024 "$f" 2>&1); then
  echo "rustfmt falhou em $f:" >&2
  echo "$out" | head -20 >&2
fi
exit 0

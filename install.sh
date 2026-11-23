#!/usr/bin/env bash
# Compila e instala o omarchy-drive para o usuário atual.
set -euo pipefail
cd "$(dirname "$0")"

# Client OAuth padrão (opcional, fora do git): permite conectar contas sem
# rodar `omarchy-drive setup`. Veja packaging/oauth.env.example.
if [ -f packaging/oauth.env ]; then
  set -a
  # shellcheck disable=SC1091
  . packaging/oauth.env
  set +a
fi

cargo build --release -p omarchy-drive
install -Dm755 target/release/omarchy-drive "$HOME/.local/bin/omarchy-drive"
install -Dm644 packaging/omarchy-drive.service "$HOME/.config/systemd/user/omarchy-drive.service"

plugin="$HOME/.config/omarchy/plugins/omarchy-drive.status"
mkdir -p "$plugin/i18n"
cp shell-plugin/manifest.json shell-plugin/Panel.qml shell-plugin/Model.js "$plugin/"
cp shell-plugin/i18n/*.json "$plugin/i18n/"

# Atalhos .gdoc/.gsheet/...: tipo MIME próprio e abertura no navegador.
data="${XDG_DATA_HOME:-$HOME/.local/share}"
install -Dm644 packaging/omarchy-drive-shortcuts.xml "$data/mime/packages/omarchy-drive-shortcuts.xml"
install -Dm644 packaging/omarchy-drive-open.desktop "$data/applications/omarchy-drive-open.desktop"
update-mime-database "$data/mime" >/dev/null 2>&1 || true
update-desktop-database "$data/applications" >/dev/null 2>&1 || true
xdg-mime default omarchy-drive-open.desktop application/x-omarchy-drive-shortcut

systemctl --user daemon-reload
echo
echo "Instalado. Próximos passos:"
echo "  1. systemctl --user enable --now omarchy-drive"
echo "  2. Adicione o widget \"Drive Sync\" à barra: omarchy bar put omarchy-drive.status --section right"
echo "  3. No painel do widget: \"Conectar Google Drive\" (ou OneDrive) e \"Sincronizar Meu Drive\"."
if [ -z "${OMARCHY_DRIVE_GOOGLE_CLIENT_ID:-}" ] && [ -z "${OMARCHY_DRIVE_MS_CLIENT_ID:-}" ]; then
  echo "     Sem client padrão embutido: antes, rode \`omarchy-drive setup google|microsoft\` (veja o README)."
fi

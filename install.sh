#!/usr/bin/env bash
# Compila e instala o omarchy-drive para o usuário atual.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release -p omarchy-drive
install -Dm755 target/release/omarchy-drive "$HOME/.local/bin/omarchy-drive"
install -Dm644 packaging/omarchy-drive.service "$HOME/.config/systemd/user/omarchy-drive.service"

plugin="$HOME/.config/omarchy/plugins/omarchy-drive.status"
mkdir -p "$plugin/i18n"
cp shell-plugin/manifest.json shell-plugin/Panel.qml shell-plugin/Model.js "$plugin/"
cp shell-plugin/i18n/*.json "$plugin/i18n/"

systemctl --user daemon-reload
echo
echo "Instalado. Próximos passos:"
echo "  1. omarchy-drive setup google --client-id ... --client-secret ...   (e/ou setup microsoft)"
echo "  2. omarchy-drive account add pessoal --provider gdrive"
echo "  3. omarchy-drive pair add drive --account pessoal --local ~/GoogleDrive"
echo "  4. systemctl --user enable --now omarchy-drive"
echo "  5. Adicione o widget \"Drive Sync\" à barra: omarchy bar put omarchy-drive.status --section right"

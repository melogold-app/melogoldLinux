#!/usr/bin/env bash
# Ставит собранный Melogold в домашнюю папку: бинарь, .desktop со ссылками melogold://,
# иконки. Нужен разработке — проверить ссылки из браузера и значок в доке без пакета.
#
#   ./scripts/install-local.sh [debug|release]      (по умолчанию release)
#   ./scripts/install-local.sh --uninstall

set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="${HOME}/.local/bin"
share="${XDG_DATA_HOME:-${HOME}/.local/share}"
app_id="app.melogold.Melogold"

refresh() {
    update-desktop-database -q "${share}/applications" 2>/dev/null || true
    # Иначе после обновления в доке остаётся старый значок (docs/PROMPT.md §9 п. 13).
    gtk-update-icon-cache -q -f -t "${share}/icons/hicolor" 2>/dev/null || true
}

if [[ ${1:-} == --uninstall ]]; then
    rm -f "${bin}/melogold" "${share}/applications/${app_id}.desktop" "${share}/metainfo/${app_id}.metainfo.xml"
    find "${share}/icons/hicolor" -name "${app_id}.png" -delete 2>/dev/null || true
    refresh
    echo "Melogold удалён из ${HOME}/.local (данные в ${share}/melogold остались)"
    exit 0
fi

profile="${1:-release}"
if [[ ${profile} == release ]]; then
    cargo build --release --manifest-path "${repo}/Cargo.toml"
else
    cargo build --manifest-path "${repo}/Cargo.toml"
fi
install -D -m 0755 "${repo}/target/${profile}/melogold" "${bin}/melogold"
install -D -m 0644 "${repo}/packaging/${app_id}.desktop" "${share}/applications/${app_id}.desktop"
install -D -m 0644 "${repo}/packaging/${app_id}.metainfo.xml" "${share}/metainfo/${app_id}.metainfo.xml"
for icon in "${repo}"/packaging/icons/hicolor/*/apps/"${app_id}".png; do
    size="$(basename "$(dirname "$(dirname "${icon}")")")"
    install -D -m 0644 "${icon}" "${share}/icons/hicolor/${size}/apps/${app_id}.png"
done
refresh
echo "Melogold ${profile} установлен: ${bin}/melogold"

#!/usr/bin/env bash
# Выпуск: AppImage, deb, rpm и update.json в dist/ — сборка в контейнере Fedora 43 (нижняя граница
# glibc), чтобы бинарь запускался на всех поддерживаемых системах (docs/PROMPT.md §3).
#
#   ./scripts/package.sh
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if ! podman image exists localhost/melogold-builder:43; then
    podman build -t melogold-builder:43 -f "${repo}/packaging/Containerfile" "${repo}/packaging"
fi
rm -f "${repo}"/dist/*.AppImage "${repo}"/dist/*.deb "${repo}"/dist/*.rpm "${repo}"/dist/update.json
podman run --rm -v "${repo}:/src:Z" \
    localhost/melogold-builder:43 bash /src/scripts/package-in-container.sh

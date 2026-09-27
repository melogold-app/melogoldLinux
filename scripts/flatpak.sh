#!/usr/bin/env bash
# Flatpak-пакет dist/Melogold.flatpak (один файл: flatpak install --user Melogold.flatpak).
# Сборка в контейнере с flatpak-builder; рантаймы и кэш — в томе podman melogold-flatpak.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if ! podman image exists localhost/melogold-flatpak:43; then
    podman build -t melogold-flatpak:43 -f "${repo}/packaging/flatpak/Containerfile" "${repo}/packaging/flatpak"
fi
podman run --rm --privileged -v "${repo}:/src:Z" -v melogold-flatpak:/var/lib/flatpak -v melogold-flatpak-cache:/cache \
    localhost/melogold-flatpak:43 bash -euo pipefail -c '
    cd /src
    tools=/cache/flatpak-builder-tools
    [[ -d ${tools} ]] || git clone --depth 1 https://github.com/flatpak/flatpak-builder-tools "${tools}"
    python3 "${tools}/cargo/flatpak-cargo-generator.py" Cargo.lock -o packaging/flatpak/cargo-sources.json
    flatpak install -y --noninteractive flathub org.gnome.Platform//49 org.gnome.Sdk//49 org.freedesktop.Sdk.Extension.rust-stable//25.08 >/dev/null
    flatpak-builder --force-clean --state-dir=/cache/state --repo=/cache/repo /cache/build packaging/flatpak/app.melogold.Melogold.yml
    mkdir -p dist
    flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo /cache/repo dist/Melogold.flatpak app.melogold.Melogold
    ls -la dist/Melogold.flatpak'

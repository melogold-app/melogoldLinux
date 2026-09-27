#!/usr/bin/env bash
# Выпуск Melogold для Linux (docs/PROMPT.md §3): проверки, пакеты в контейнере Fedora 43 и — с
# --publish — релиз vX.Y.Z на GitHub. Публикация атомарна: черновик → файлы → последний релиз,
# поэтому update.json не появится раньше самих пакетов.
#
#   ./scripts/release.sh             собрать dist/
#   ./scripts/release.sh --publish   и выпустить
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
tag="v${version}"
for lang in ru en; do
    [[ -s release-notes/${version}.${lang}.md ]] || { echo "нет release-notes/${version}.${lang}.md"; exit 1; }
done
grep -q "<release version=\"${version}\"" packaging/app.melogold.Melogold.metainfo.xml ||
    { echo "в metainfo нет <release version=\"${version}\">"; exit 1; }

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --quiet
./scripts/package.sh

[[ ${1:-} == --publish ]] || { echo "dist/ готов; выпуск — с --publish"; exit 0; }
[[ -z $(git status --porcelain) ]] || { echo "есть незафиксированные правки"; exit 1; }
git fetch --quiet origin
[[ $(git rev-parse HEAD) == $(git rev-parse origin/main) ]] || { echo "HEAD не совпадает с origin/main: сначала push"; exit 1; }
gh release view "${tag}" >/dev/null 2>&1 && { echo "релиз ${tag} уже есть"; exit 1; }

notes="$(mktemp)"
{ cat "release-notes/${version}.ru.md"; echo; echo "---"; echo; cat "release-notes/${version}.en.md"; } >"${notes}"
gh release create "${tag}" --draft --title "Melogold ${version}" --notes-file "${notes}" --target "$(git rev-parse HEAD)" \
    dist/Melogold-x86_64.AppImage "dist/melogold_${version}_amd64.deb" dist/melogold-"${version}"-1.*.x86_64.rpm
gh release upload "${tag}" dist/update.json
gh release edit "${tag}" --draft=false --latest
echo "выпущено: $(gh release view "${tag}" --json url -q .url)"

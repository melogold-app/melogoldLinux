#!/usr/bin/env bash
# Снимки окна Melogold в отдельном безэкранном композиторе (docs/PROMPT.md §3 «Снимки окна»).
#
#   ./scripts/snapshots.sh [папка] [размер тема язык]…
#   ./scripts/snapshots.sh target/snapshots "360x640 dark ru"
#
# Окно открывается во вложенном weston (--backend=headless), а не в сеансе пользователя:
# он в это время работает, и окно, забравшее фокус, увело бы его набор (грабли §9 п. 15).
# Данные — во временных папках XDG: снимки не трогают ни библиотеку, ни настройки.

set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="${1:-${repo}/target/snapshots}"
shift || true
configs=("$@")
if [[ ${#configs[@]} -eq 0 ]]; then
    configs=("1280x800 light ru" "1280x800 dark en" "800x600 light ru" "360x640 light ru" "360x640 dark en")
fi

cargo build --quiet -p melogold-gtk --manifest-path "${repo}/Cargo.toml"

runtime="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
socket="melogold-shots-$$"
home="$(mktemp -d)"
weston --backend=headless --renderer=pixman --socket="${socket}" --width=1600 --height=1200 --idle-time=0 \
    >"${home}/weston.log" 2>&1 &
weston_pid=$!
cleanup() {
    kill "${weston_pid}" 2>/dev/null || true
    rm -rf "${home}"
}
trap cleanup EXIT
for _ in $(seq 50); do
    [[ -S "${runtime}/${socket}" ]] && break
    sleep 0.1
done
[[ -S "${runtime}/${socket}" ]] || { cat "${home}/weston.log"; echo "weston не поднялся"; exit 1; }

for config in "${configs[@]}"; do
    read -r size scheme lang <<<"${config}"
    dir="${out}/${size}-${scheme}-${lang}"
    rm -rf "${dir}"
    env -u DISPLAY WAYLAND_DISPLAY="${socket}" GDK_BACKEND=wayland \
        XDG_DATA_HOME="${home}/data" XDG_CONFIG_HOME="${home}/config" XDG_CACHE_HOME="${home}/cache" \
        MELOGOLD_SCREENSHOT_DIR="${dir}" MELOGOLD_SCREENSHOT_SIZE="${size}" \
        MELOGOLD_SCREENSHOT_SCHEME="${scheme}" MELOGOLD_SCREENSHOT_LANG="${lang}" \
        MELOGOLD_AUDIO_SINK=fakesink MELOGOLD_FAKE_GEO=cYKAr38pZcY:RU RUST_LOG="${RUST_LOG:-warn}" \
        timeout 120 "${repo}/target/debug/melogold"
done
echo "снимки: ${out}"

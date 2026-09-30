#!/usr/bin/env bash
# Сборка выпуска внутри контейнера melogold-builder:43 (scripts/package.sh): бинарь, AppImage, deb,
# rpm — из одного дерева установки, чтобы пакеты не расходились составом.
set -euo pipefail

repo=/src
cd "${repo}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
app_id=app.melogold.Melogold
out="${repo}/dist"
cache="${repo}/dist/.cache"
mkdir -p "${out}" "${cache}"

export CARGO_TARGET_DIR="${repo}/target/fedora43"
export CARGO_HOME="${repo}/target/cargo-home"
cargo build --release --locked
binary="${CARGO_TARGET_DIR}/release/melogold"
strip "${binary}"

# ── дерево установки (/usr) ──────────────────────────────────────────────────────
stage="$(mktemp -d)"
install -D -m 0755 "${binary}" "${stage}/usr/bin/melogold"
# Помощник обновления deb и rpm (запускается окном через pkexec) и его polkit-действие.
helper="${CARGO_TARGET_DIR}/release/melogold-update"
strip "${helper}"
install -D -m 0755 "${helper}" "${stage}/usr/libexec/melogold/melogold-update"
install -D -m 0644 "packaging/polkit/${app_id}.update.policy" "${stage}/usr/share/polkit-1/actions/${app_id}.update.policy"
install -D -m 0644 "packaging/${app_id}.desktop" "${stage}/usr/share/applications/${app_id}.desktop"
install -D -m 0644 "packaging/${app_id}.metainfo.xml" "${stage}/usr/share/metainfo/${app_id}.metainfo.xml"
for icon in packaging/icons/hicolor/*/apps/"${app_id}".png; do
    size="$(basename "$(dirname "$(dirname "${icon}")")")"
    install -D -m 0644 "${icon}" "${stage}/usr/share/icons/hicolor/${size}/apps/${app_id}.png"
done
install -D -m 0644 LICENSE "${stage}/usr/share/licenses/melogold/LICENSE"
desktop-file-validate "${stage}/usr/share/applications/${app_id}.desktop"
appstreamcli validate --no-net "${stage}/usr/share/metainfo/${app_id}.metainfo.xml"

# ── rpm ──────────────────────────────────────────────────────────────────────────
rpmbuild -bb --quiet \
    --define "_topdir ${cache}/rpmbuild" \
    --define "_rpmdir ${out}" \
    --define "melogold_version ${version}" \
    --define "melogold_stage ${stage}" \
    packaging/rpm/melogold.spec
find "${out}" -mindepth 2 -name 'melogold-*.rpm' -exec mv {} "${out}/" \;
find "${out}" -mindepth 1 -maxdepth 1 -type d ! -name '.cache' -exec rm -rf {} +

# ── deb ──────────────────────────────────────────────────────────────────────────
deb="$(mktemp -d)"
cp -a "${stage}/." "${deb}/"
mkdir -p "${deb}/DEBIAN" "${deb}/usr/share/doc/melogold"
mv "${deb}/usr/share/licenses/melogold/LICENSE" "${deb}/usr/share/doc/melogold/copyright"
rm -rf "${deb}/usr/share/licenses"
installed_kb="$(du -sk "${deb}/usr" | cut -f1)"
sed -e "s/@VERSION@/${version}/" -e "s/@SIZE@/${installed_kb}/" packaging/deb/control > "${deb}/DEBIAN/control"
dpkg-deb --root-owner-group --build "${deb}" "${out}/melogold_${version}_amd64.deb" >/dev/null

# ── AppImage ─────────────────────────────────────────────────────────────────────
appdir="$(mktemp -d)/AppDir"
mkdir -p "${appdir}"
cp -a "${stage}/usr" "${appdir}/"
# AppImage обновляет себя сам, помощник и polkit ему не нужны.
rm -rf "${appdir}/usr/libexec/melogold" "${appdir}/usr/share/polkit-1"
install -m 0755 packaging/appimage/AppRun "${appdir}/AppRun"
cp "packaging/${app_id}.desktop" "${appdir}/${app_id}.desktop"
cp "packaging/icons/hicolor/256x256/apps/${app_id}.png" "${appdir}/${app_id}.png"
ln -s "${app_id}.png" "${appdir}/.DirIcon"

# GStreamer: только то, из чего собран конвейер (output.rs) и выбор вывода звука.
mkdir -p "${appdir}/usr/lib/gstreamer-1.0" "${appdir}/usr/libexec/gstreamer-1.0"
# Декодер AAC — fdk-aac (gst-plugins-bad): gst-libav тянет за собой ffmpeg с сотней библиотек.
for plugin in coreelements app audioconvert audioresample audiofx volume pulseaudio autodetect alsa fdkaac; do
    cp "/usr/lib64/gstreamer-1.0/libgst${plugin}.so" "${appdir}/usr/lib/gstreamer-1.0/"
done
cp /usr/libexec/gstreamer-1.0/gst-plugin-scanner "${appdir}/usr/libexec/gstreamer-1.0/"

# Схемы GTK: без них GtkFileChooser и настройки падают на системах без GTK 4.
mkdir -p "${appdir}/usr/share/glib-2.0/schemas"
cp /usr/share/glib-2.0/schemas/org.gtk.gtk4.*.xml "${appdir}/usr/share/glib-2.0/schemas/" 2>/dev/null || true
glib-compile-schemas "${appdir}/usr/share/glib-2.0/schemas"

# Символические значки Adwaita — запасные: на KDE и в минимальных системах их нет.
mkdir -p "${appdir}/usr/share/icons/Adwaita"
cp /usr/share/icons/Adwaita/index.theme "${appdir}/usr/share/icons/Adwaita/"
cp -r /usr/share/icons/Adwaita/symbolic "${appdir}/usr/share/icons/Adwaita/"
mkdir -p "${appdir}/usr/lib/gio/modules"

# Библиотеки: всё, что просят бинарь и плагины, кроме системного (excludelist AppImage).
excludelist="${cache}/excludelist"
if [[ ! -s ${excludelist} ]]; then
    curl -fsSL https://raw.githubusercontent.com/AppImageCommunity/pkg2appimage/master/excludelist -o "${excludelist}"
fi
excluded="$(grep -v '^\s*#' "${excludelist}" | sed 's/\s*#.*//' | grep -v '^\s*$' | sort -u)"
declare -A seen=()
queue=("${appdir}/usr/bin/melogold" "${appdir}"/usr/lib/gstreamer-1.0/*.so "${appdir}/usr/libexec/gstreamer-1.0/gst-plugin-scanner")
while ((${#queue[@]})); do
    item="${queue[0]}"
    queue=("${queue[@]:1}")
    while read -r name path; do
        [[ -n ${path} && -f ${path} ]] || continue
        grep -qxF "${name}" <<<"${excluded}" && continue
        [[ -n ${seen[${name}]:-} ]] && continue
        seen[${name}]=1
        cp -L "${path}" "${appdir}/usr/lib/${name}"
        queue+=("${appdir}/usr/lib/${name}")
    done < <(ldd "${item}" 2>/dev/null | awk '/=>/ { print $1, $3 }')
done
echo "библиотек в AppImage: ${#seen[@]}"

appimagetool="${cache}/appimagetool-x86_64.AppImage"
if [[ ! -x ${appimagetool} ]]; then
    curl -fsSL https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage -o "${appimagetool}"
    chmod +x "${appimagetool}"
fi
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "${appimagetool}" --no-appstream "${appdir}" "${out}/Melogold-x86_64.AppImage" >/dev/null
chmod +x "${out}/Melogold-x86_64.AppImage"

# ── update.json: формат Android плюс файл по архитектуре ─────────────────────────
# assets (AppImage) читают и 0.1.3 и старше — не менять; packages (deb, rpm) старые версии пропускают.
python3 - "${out}" "${version}" <<'PY'
import hashlib, json, os, sys, glob, datetime

out, version = sys.argv[1], sys.argv[2]

def asset(path):
    data = open(path, "rb").read()
    return {"fileName": os.path.basename(path), "sizeBytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}

def note(lang):
    path = f"release-notes/{version}.{lang}.md"
    return open(path, encoding="utf-8").read().strip() if os.path.exists(path) else None

rpm, = glob.glob(f"{out}/melogold-{version}-1.*.x86_64.rpm")
manifest = {
    "version": version,
    "notes": {"ru": note("ru"), "en": note("en")},
    "publishedAt": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "assets": {"x86_64": asset(f"{out}/Melogold-x86_64.AppImage")},
    "packages": {
        "deb": {"x86_64": asset(f"{out}/melogold_{version}_amd64.deb")},
        "rpm": {"x86_64": asset(rpm)},
    },
}
with open(f"{out}/update.json", "w", encoding="utf-8") as f:
    json.dump(manifest, f, ensure_ascii=False, indent=2)
    f.write("\n")
PY
ls -la "${out}"

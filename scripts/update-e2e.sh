#!/usr/bin/env bash
# Живая проверка помощника обновления (docs/PROMPT.md §3 «Обновления») в чистых контейнерах:
# ставит собранный dist/ пакет, делает из него «следующую версию» (0.0.+1 к последней цифре) и запускает
# `melogold-update` от root напрямую с подложенным манифестом (MELOGOLD_UPDATE_SOURCE — только без PKEXEC_UID).
#
#   ./scripts/update-e2e.sh                  # Fedora 44 и Ubuntu 25.10
#   ./scripts/update-e2e.sh fedora:44        # один образ
#
# Нужны: podman, собранный ./scripts/package.sh и образ localhost/melogold-builder:43 (rpmbuild, dpkg-deb).
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "${repo}/Cargo.toml" | head -1)"
next="${version%.*}.$((${version##*.} + 1))"
work="${repo}/target/update-e2e"
images=("$@")
[[ ${#images[@]} -gt 0 ]] || images=(registry.fedoraproject.org/fedora:44 docker.io/library/ubuntu:25.10)

# ── «следующая версия»: те же файлы под номером ${next}, манифесты: годный, битый sha256, та же версия ──
rm -rf "${work}"
mkdir -p "${work}"
podman run --rm -v "${repo}:/src:Z" -e version="${version}" -e next="${next}" \
    localhost/melogold-builder:43 bash -euo pipefail -c '
    out=/src/target/update-e2e
    cd "$out"
    cp /src/dist/melogold-${version}-1.*.x86_64.rpm /src/dist/melogold_${version}_amd64.deb .
    # rpm: то же дерево, другая версия
    stage=$(mktemp -d); (cd "$stage" && rpm2cpio /src/dist/melogold-${version}-1.*.x86_64.rpm | cpio -idm --quiet)
    rpmbuild -bb --quiet --define "_topdir /tmp/rpmbuild" --define "_rpmdir $out/rpm" \
        --define "melogold_version ${next}" --define "melogold_stage $stage" /src/packaging/rpm/melogold.spec
    mv $out/rpm/*/*.rpm . && rm -rf $out/rpm
    # deb: распаковать, поправить версию, собрать
    d=$(mktemp -d); dpkg-deb -R /src/dist/melogold_${version}_amd64.deb "$d"
    sed -i "s/^Version: .*/Version: ${next}/" "$d/DEBIAN/control"
    dpkg-deb --root-owner-group --build "$d" "melogold_${next}_amd64.deb" >/dev/null
    chmod -R a+rX "$out"
'
python3 - "${work}" "${next}" "${version}" <<'PY'
import glob, hashlib, json, os, shutil, sys
work, nxt, cur = sys.argv[1:]
def asset(path):
    data = open(path, "rb").read()
    return {"fileName": os.path.basename(path), "sizeBytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
rpm, = glob.glob(f"{work}/melogold-{nxt}-1.*.x86_64.rpm")
deb = f"{work}/melogold_{nxt}_amd64.deb"
def manifest(version, bad=False):
    m = {"version": version, "notes": {}, "assets": {},
         "packages": {"deb": {"x86_64": asset(deb)}, "rpm": {"x86_64": asset(rpm)}}}
    if bad:
        for fmt in m["packages"].values():
            fmt["x86_64"]["sha256"] = "0" * 64
    return m
for name, m in {"good": manifest(nxt), "badsha": manifest(nxt, True), "same": manifest(cur)}.items():
    os.makedirs(f"{work}/{name}", exist_ok=True)
    for f in (rpm, deb):
        shutil.copy(f, f"{work}/{name}/")
    json.dump(m, open(f"{work}/{name}/update.json", "w"))
PY

# ── внутри контейнера ───────────────────────────────────────────────────────────
inside="$(cat <<'INSIDE'
set -uo pipefail
fail=0
check() { if eval "$2"; then echo "  ok   $1"; else echo "  FAIL $1"; fail=1; fi; }
helper=/usr/libexec/melogold/melogold-update
if command -v dnf >/dev/null; then
    fmt=rpm
    dnf install -y -q "/w/melogold-${version}-1."*.rpm >/dev/null 2>&1
    installed() { rpm -q --qf '%{VERSION}' melogold; }
    file_next=$(ls /w/melogold-${next}-1.*.rpm)
    file_cur=$(ls /w/melogold-${version}-1.*.rpm)
    downgrade() { dnf downgrade -y -q "$file_cur" >/dev/null 2>&1; }
else
    fmt=deb
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq >/dev/null 2>&1
    apt-get install -y -qq "/w/melogold_${version}_amd64.deb" >/dev/null 2>&1
    installed() { dpkg-query -W -f='${Version}' melogold; }
    file_next=/w/melogold_${next}_amd64.deb
    file_cur=/w/melogold_${version}_amd64.deb
    downgrade() { apt-get install -y -qq --allow-downgrades "$file_cur" >/dev/null 2>&1; }
fi
echo "== $(. /etc/os-release; echo "$PRETTY_NAME"), формат ${fmt}"
check "установлена ${version}" '[[ $(installed) == "$version" ]]'
check "помощник на месте, 0755 root" '[[ $(stat -c "%a %U" $helper) == "755 root" ]]'
policy=/usr/share/polkit-1/actions/app.melogold.Melogold.update.policy
check "policy на месте" '[[ -f $policy ]]'
if command -v xmllint >/dev/null; then check "xmllint policy" 'xmllint --noout $policy'; fi
if command -v pkaction >/dev/null; then
    pkaction --verbose --action-id app.melogold.Melogold.update | sed 's/^/       /'
    check "pkaction: allow_active=yes, auth_admin вне сеанса" \
        'o=$(pkaction --verbose --action-id app.melogold.Melogold.update); grep -q "implicit active:.*yes" <<<"$o" && grep -q "implicit any:.*auth_admin" <<<"$o" && grep -q "exec.path.*$helper" <<<"$o"'
else
    echo "  --   pkaction нет (polkit не поставился?)"; fail=1
fi
check "pkexec на месте" 'command -v pkexec >/dev/null'

run() { out=$("$@" 2>/tmp/err); code=$?; echo "       код $code: $out"; }

echo "-- не root: отказ"
run setpriv --reuid=65534 --regid=65534 --clear-groups env MELOGOLD_UPDATE_SOURCE=/w/good $helper
check "код 2, версия прежняя" '[[ $code == 2 && $(installed) == "$version" ]]'

echo "-- подменённый sha256: ничего не ставится"
run env MELOGOLD_UPDATE_SOURCE=/w/badsha $helper "$file_next"
check "код 6 checksum, версия прежняя, рабочий каталог пуст" '[[ $code == 6 && $out == "error checksum"* && $(installed) == "$version" && -z $(ls -A /var/lib/melogold-update 2>/dev/null) ]]'

echo "-- версия не новее"
run env MELOGOLD_UPDATE_SOURCE=/w/same $helper "$file_next"
check "код 3 not-newer" '[[ $code == 3 && $(installed) == "$version" ]]'

echo "-- под pkexec (PKEXEC_UID есть) папка-подмена игнорируется: манифест берётся с GitHub"
run env PKEXEC_UID=1000 MELOGOLD_UPDATE_SOURCE=/w/good $helper "$file_next"
check "версия прежняя (с GitHub: не новее или нет связи)" '[[ $code != 0 && $(installed) == "$version" ]]'

echo "-- лишний аргумент и чужой файл окна"
run env MELOGOLD_UPDATE_SOURCE=/w/good $helper a b
check "код 2" '[[ $code == 2 ]]'
ln -sf "$file_next" /tmp/link; echo garbage > /tmp/garbage
run env MELOGOLD_UPDATE_SOURCE=/w/good $helper /tmp/garbage
check "мусор вместо файла окна → скачал сам, поставил ${next}" '[[ $code == 0 && $out == "ok $next" && $(installed) == "$next" ]]'
check "файл из рабочего каталога убран" '[[ ! -d /var/lib/melogold-update || -z $(ls -A /var/lib/melogold-update) ]]'

echo "-- вернуть ${version} и поставить с настоящим файлом окна (через символическую ссылку — отклоняется, скачивается сам)"
downgrade
check "вернулась ${version}" '[[ $(installed) == "$version" ]]'
run env MELOGOLD_UPDATE_SOURCE=/w/good $helper /tmp/link
check "ссылка не читается, поставил ${next} из источника" '[[ $code == 0 && $(installed) == "$next" ]]'
downgrade
cp "$file_next" /tmp/from-window."$fmt"
run env MELOGOLD_UPDATE_SOURCE=/w/good $helper /tmp/from-window."$fmt"
check "файл окна принят, поставил ${next}" '[[ $code == 0 && $out == "ok $next" && $(installed) == "$next" ]]'
exit $fail
INSIDE
)"
status=0
for image in "${images[@]}"; do
    podman run --rm -v "${work}:/w:ro,Z" -e version="${version}" -e next="${next}" "${image}" bash -c "${inside}" || status=1
done
exit ${status}

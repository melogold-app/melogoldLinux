# 0015 · Обновление deb и rpm кнопкой «Обновить»

Статус: сделано

## Что нужно пользователю

Как в Windows-версии: вышла новая версия — в Настройках «Обновить», без пароля и без похода на GitHub. Раньше так обновлялся только AppImage, а у deb и rpm «Скачать» открывало страницу релиза.

## Решение

Описание — `docs/PROMPT.md` §3 «Обновления».

- `update.json` (`scripts/package-in-container.sh`): `assets.x86_64` — AppImage, как раньше (0.1.3 и старше читают его), новый ключ `packages.deb` и `packages.rpm` — файлы с `sizeBytes` и `sha256`.
- Помощник `crates/melogold-update` → `/usr/libexec/melogold/melogold-update`, под root; окну верит только в путь к скачанному файлу.
- polkit: `packaging/polkit/app.melogold.Melogold.update.policy`, `allow_active=yes`.
- Окно (`crates/melogold-gtk/src/updates.rs`): формат по `rpm -qf`/`dpkg -S` для `/usr/bin/melogold`; скачивание → `pkexec` → перезапуск. Flatpak и неопознанное — «Скачать».
- С 0.1.3 на 0.1.4 — один раз через «Скачать»: 0.1.3 помощника не знает.

## Проверка

- `cargo test --workspace`: разбор манифеста старого и нового формата, выбор файла по формату, отказ при неверных размере, sha256 и адресе, «версия не новее».
- `./scripts/update-e2e.sh`: чистые Fedora 44 и Ubuntu 25.10, пакет ставится, помощник от root обновляет до «следующей версии», подменённый sha256 ничего не ставит.
- Снимки: `MELOGOLD_UPDATES=1 MELOGOLD_UPDATE_FORMAT=rpm MELOGOLD_SNAPSHOT_STEPS=04 ./scripts/snapshots.sh`.

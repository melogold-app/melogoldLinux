# Проверка выпусков 0.1.0 и 0.1.1

Записано 27.09.2026. Пакеты — из `scripts/package.sh` и `scripts/flatpak.sh`, то есть те же файлы, что лежат в
[GitHub Releases](https://github.com/melogold-app/melogoldLinux/releases).

## Стенд

| Где | Система | Окружение | Что ставилось |
|---|---|---|---|
| `cl-fedora` (VM) | Fedora 44 | GNOME 49, Wayland | `.rpm` через `dnf install` |
| `cl-arch` (VM) | Arch Linux | KDE Plasma, Wayland | AppImage (fuse3, без fuse2) |
| `cl-ubuntu` (VM) | Ubuntu 24.04.4 LTS | GNOME 46, Wayland | `Melogold.flatpak` (рантайм GNOME 49) |
| контейнер | Ubuntu 25.10 | — | `.deb` через `apt install`, `ldd` без пропусков |
| рабочая машина | Fedora 44 | вложенный weston | AppImage |

Окно снималось своим механизмом снимков (`MELOGOLD_SCREENSHOT_DIR`, шаги `scripts/snapshots.sh`) в сеансе VM, звук —
`MELOGOLD_AUDIO_SINK=fakesink`. Экраны не снимались.

## Что проверено

| Проверка | Fedora rpm | Arch AppImage | Ubuntu Flatpak |
|---|---|---|---|
| Окно, разделы, Настройки, «Сейчас играет», текст песни | да | да | да |
| Звук: поток, декодер AAC, «от нажатия до звука» | 113 мс (libav) | 145 мс (fdk-aac) | 114 мс (codecs-extra) |
| Значки: все на месте в чужой теме | Adwaita | Breeze + свои запасные | Adwaita |
| Встраивание в меню и `melogold://` | пакет | `.desktop` с `Exec` на AppImage | Flatpak |

### Обновление 0.1.0 → 0.1.1 (AppImage, Arch)

1. AppImage 0.1.0 скачан с GitHub в `~/Applications`, SHA-256 совпал с `update.json` релиза.
2. После выпуска 0.1.1 запуск 0.1.0: через 5 с проверка нашла 0.1.1 (`updates.announcedVersion = 0.1.1`,
   уведомление).
3. «Обновить» в полосе Настроек и «Обновить» в «Что нового» (нажаты через AT-SPI, по имени кнопки) — скачивание,
   сверка размера и SHA-256, замена файла, перезапуск: в журнале `обновление установлено, перезапуск версия=0.1.1`, затем
   `Melogold 0.1.1`.
4. Файл AppImage — ровно `sha256` из `update.json` 0.1.1; пункт меню переписан (`X-AppImage-Version=0.1.1`).

## Что нашлось и исправлено до выпуска

1. **AppImage весил 114 МБ.** gst-libav тянул ffmpeg со всеми зависимостями (tesseract, flite, samba). Декодер AAC в
   AppImage — fdk-aac из gst-plugins-bad: 41 МБ.
2. **В KDE не хватало значков.** В Breeze нет части имён Adwaita (`view-list-bullet-symbolic`), и GTK рисовал значок
   «нет картинки». Символические значки, которые использует окно, теперь в ресурсах приложения
   (`scripts/sync-symbolic-icons.py`).
3. **`emblem-synchronizing-symbolic` в Adwaita больше нет** — пусто было и в GNOME. Заменён на `view-refresh-symbolic`.
4. **rpm тянул ffmpeg.** Зависимость на декодер теперь сначала `gstreamer1-plugins-bad-free` (обычно уже стоит).

## Что осталось на стороне YouTube

С адреса стенда (страна по YouTube — NL) часть треков отвечает `LOGIN_REQUIRED` «Sign in to confirm you're not a bot»
у клиента VISIONOS — единственного в `config/stream-clients.json` (у Windows так же). Играют не все треки. Список
клиентов обновляется без выпуска, через этот файл.

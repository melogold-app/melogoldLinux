#!/usr/bin/env python3
"""Иконки приложения — скруглённая плитка Windows-клиента (docs/PROMPT.md §5.1, §11 п. 12).

    ./scripts/sync-icon.py [путь к melogoldWindows]

Источник — `tools/melogold-tile.png` Windows (512×512, белая плитка со скруглением 22 %
и грейпфрут на ней). Плитка переносится как есть, с фоном и скруглёнными углами:
пользователь хочет один и тот же значок на всех системах (2026-09-27), а не вырезанный
плод без подложки. Размеры пишутся готовыми файлами в дерево, чтобы установке и сборке
пакетов не нужен был Pillow.
"""

import pathlib
import sys

try:
    from PIL import Image
except ImportError:
    sys.exit("нужен Pillow: sudo dnf install -y python3-pillow")

ICON_NAME = "app.melogold.Melogold"
# Стандартный набор hicolor; без 256 и 512 GNOME растягивает мелкий размер, и выходит мыло.
SIZES = [16, 24, 32, 48, 64, 128, 256, 512]

repo = pathlib.Path(__file__).resolve().parent.parent
windows = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else repo.parent / "melogoldWindows"
source = windows / "tools" / "melogold-tile.png"
if not source.is_file():
    sys.exit(f"не нашёл плитку: {source}")

tile = Image.open(source).convert("RGBA")
if tile.size[0] != tile.size[1]:
    sys.exit("плитка должна быть квадратной")

hicolor = repo / "packaging" / "icons" / "hicolor"
for size in SIZES:
    folder = hicolor / f"{size}x{size}" / "apps"
    folder.mkdir(parents=True, exist_ok=True)
    for old in folder.glob("*.png"):
        if old.stem != ICON_NAME:
            old.unlink()
    tile.resize((size, size), Image.LANCZOS).save(folder / f"{ICON_NAME}.png", optimize=True)

# Та же плитка вшита в ресурсы: «О приложении» и окно показывают её и без установки.
bundled = repo / "crates" / "melogold-gtk" / "resources" / "icons" / "256x256" / "apps" / f"{ICON_NAME}.png"
bundled.parent.mkdir(parents=True, exist_ok=True)
tile.resize((256, 256), Image.LANCZOS).save(bundled, optimize=True)
print(f"{len(SIZES)} размеров → {hicolor.relative_to(repo)} (из {source})")

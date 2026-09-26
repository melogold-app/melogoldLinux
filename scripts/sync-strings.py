#!/usr/bin/env python3
"""Строки интерфейса из общего каталога Windows-клиента (docs/PROMPT.md §3 «Строки»).

    ./scripts/sync-strings.py [путь к melogoldWindows]

Источник — `src/Melogold.App/Strings/{ru-RU,en-US}/Resources.resw` Windows (тексты по GLOSSARY
Android): по ним работает приложение Windows. `tools/strings.tsv` бывает старше resw — в сентябре
2026 там ещё не было текстов задания «Трек закрыт в стране», — поэтому он не источник.
Результат — `crates/melogold-gtk/src/strings_generated.rs`: отсортированная таблица для
двоичного поиска. Строки, которых у Windows нет, живут в `LINUX_ONLY`
(`localization.rs`) и сюда не попадают.
"""

import pathlib
import subprocess
import sys
import xml.etree.ElementTree as ElementTree

repo = pathlib.Path(__file__).resolve().parent.parent
windows = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else repo.parent / "melogoldWindows"
strings = windows / "src" / "Melogold.App" / "Strings"
target = repo / "crates" / "melogold-gtk" / "src" / "strings_generated.rs"

try:
    commit = subprocess.run(
        ["git", "-C", str(windows), "rev-parse", "--short", "HEAD"], capture_output=True, text=True, check=True
    ).stdout.strip()
except (OSError, subprocess.CalledProcessError):
    commit = "неизвестен"


def resw(language: str) -> dict:
    root = ElementTree.parse(strings / language / "Resources.resw").getroot()
    return {data.get("name"): data.findtext("value") or "" for data in root.iter("data")}


ru, en = resw("ru-RU"), resw("en-US")
rows = {}
for key in sorted(set(ru) | set(en)):
    # Ключ есть только в одном языке — берём его текст для обоих: пустая подпись хуже чужого языка.
    rows[key] = (ru.get(key, en.get(key)), en.get(key, ru.get(key)))


def literal(text: str) -> str:
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n") + '"'


# Порядок — по байтам UTF-8, как у `str::cmp` в Rust: на нём держится двоичный поиск.
keys = sorted(rows, key=lambda k: k.encode("utf-8"))
lines = [
    "// Создано scripts/sync-strings.py — руками не править.",
    f"// Источник: melogoldWindows/src/Melogold.App/Strings/*/Resources.resw, коммит {commit}.",
    "",
    "/// (ключ, русский, английский), по возрастанию ключа.",
    "pub static STRINGS: &[(&str, &str, &str)] = &[",
]
for key in keys:
    ru, en = rows[key]
    lines.append(f"    ({literal(key)}, {literal(ru)}, {literal(en)}),")
lines.append("];")
target.write_text("\n".join(lines) + "\n", encoding="utf-8")
print(f"{len(keys)} строк → {target.relative_to(repo)} (Windows {commit})")

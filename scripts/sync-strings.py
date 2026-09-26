#!/usr/bin/env python3
"""Строки интерфейса из общего каталога Windows-клиента (docs/PROMPT.md §3 «Строки»).

    ./scripts/sync-strings.py [путь к melogoldWindows]

Источник — `tools/strings.tsv` Windows (ключ, русский, английский; тексты по GLOSSARY
Android). Результат — `crates/melogold-gtk/src/strings_generated.rs`: отсортированная
таблица для двоичного поиска. Строки, которых у Windows нет, живут в `LINUX_ONLY`
(`localization.rs`) и сюда не попадают.
"""

import pathlib
import subprocess
import sys

repo = pathlib.Path(__file__).resolve().parent.parent
windows = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else repo.parent / "melogoldWindows"
source = windows / "tools" / "strings.tsv"
target = repo / "crates" / "melogold-gtk" / "src" / "strings_generated.rs"

try:
    commit = subprocess.run(
        ["git", "-C", str(windows), "rev-parse", "--short", "HEAD"], capture_output=True, text=True, check=True
    ).stdout.strip()
except (OSError, subprocess.CalledProcessError):
    commit = "неизвестен"

rows = {}
for number, line in enumerate(source.read_text(encoding="utf-8").splitlines(), 1):
    if not line.strip() or line.startswith("#"):
        continue
    parts = line.split("\t")
    if len(parts) != 3:
        sys.exit(f"{source}:{number}: ждал три колонки, получил {len(parts)}")
    key, ru, en = parts
    if key in rows:
        sys.exit(f"{source}:{number}: ключ {key} повторяется")
    rows[key] = (ru, en)


def literal(text: str) -> str:
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"') + '"'


# Порядок — по байтам UTF-8, как у `str::cmp` в Rust: на нём держится двоичный поиск.
keys = sorted(rows, key=lambda k: k.encode("utf-8"))
lines = [
    "// Создано scripts/sync-strings.py — руками не править.",
    f"// Источник: melogoldWindows/tools/strings.tsv, коммит {commit}.",
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

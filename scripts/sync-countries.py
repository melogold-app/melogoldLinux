#!/usr/bin/env python3
"""Названия стран по коду из двух букв, русские и английские (задание 0001: «Недоступно в стране «Россия»»).

    ./scripts/sync-countries.py [тег CLDR]

Источник — CLDR (unicode-org/cldr-json, `cldr-localenames-full`), те же названия, что у ICU, которым
пользуется Windows. Таблица вшивается в код: ICU есть не в каждой сборке Linux (AppImage, Flatpak),
а язык названия должен совпадать с языком интерфейса Melogold, а не системы.
"""

import json
import pathlib
import sys
import urllib.request

TAG = sys.argv[1] if len(sys.argv) > 1 else "48.2.2"
BASE = f"https://raw.githubusercontent.com/unicode-org/cldr-json/{TAG}/cldr-json/cldr-localenames-full/main"
repo = pathlib.Path(__file__).resolve().parent.parent
target = repo / "crates" / "melogold-core" / "src" / "countries_generated.rs"


def territories(language: str) -> dict:
    with urllib.request.urlopen(f"{BASE}/{language}/territories.json", timeout=30) as response:
        data = json.load(response)
    return data["main"][language]["localeDisplayNames"]["territories"]


ru, en = territories("ru"), territories("en")
codes = sorted(c for c in en if len(c) == 2 and c.isalpha() and c.isupper() and c in ru)
lines = [
    "// Создано scripts/sync-countries.py — руками не править.",
    f"// Источник: CLDR {TAG}, cldr-localenames-full/main/{{ru,en}}/territories.json.",
    "",
    "/// (код, русский, английский), по возрастанию кода.",
    "pub static COUNTRIES: &[(&str, &str, &str)] = &[",
]
for code in codes:
    lines.append(f'    ("{code}", "{ru[code]}", "{en[code]}"),')
lines.append("];")
target.write_text("\n".join(lines) + "\n", encoding="utf-8")
print(f"{len(codes)} стран → {target.relative_to(repo)} (CLDR {TAG})")

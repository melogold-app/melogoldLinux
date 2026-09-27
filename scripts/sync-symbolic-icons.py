#!/usr/bin/env python3
"""Символические значки Adwaita, которые использует Melogold, — в ресурсы приложения.

В чужой теме (Breeze в KDE, Papirus) части имён Adwaita нет, и GTK рисует на их месте значок
«нет картинки». Значок из ресурсов находится всегда; тема, где он есть, по-прежнему важнее.

    ./scripts/sync-symbolic-icons.py      (нужна тема Adwaita: /usr/share/icons/Adwaita)
"""
import pathlib
import re

repo = pathlib.Path(__file__).resolve().parent.parent
src = repo / "crates/melogold-gtk/src"
resources = repo / "crates/melogold-gtk/resources"
target = resources / "icons/scalable/actions"
adwaita = pathlib.Path("/usr/share/icons/Adwaita/symbolic")

names = set()
for path in src.rglob("*.rs"):
    names.update(re.findall(r'"([a-z0-9-]+-symbolic)"', path.read_text(encoding="utf-8")))
own = {p.stem for p in target.glob("*.svg")}
copied = []
for name in sorted(names):
    found = next(adwaita.glob(f"*/{name}.svg"), None)
    if found is None:
        continue
    destination = target / f"{name}.svg"
    # Свой значок с тем же именем не заменяется.
    if destination.exists() and destination.read_bytes() != found.read_bytes():
        continue
    destination.write_bytes(found.read_bytes())
    copied.append(name)

xml = resources / "resources.gresource.xml"
text = xml.read_text(encoding="utf-8")
lines = [f'    <file preprocess="xml-stripblanks">icons/scalable/actions/{p.name}</file>' for p in sorted(target.glob("*.svg"))]
text = re.sub(r'(    <file preprocess="xml-stripblanks">icons/scalable/actions/[^<]+</file>\n)+', "\n".join(lines) + "\n", text, count=1)
xml.write_text(text, encoding="utf-8")
missing = sorted(n for n in names if n not in copied and n not in own)
print(f"из Adwaita: {len(copied)}; свои: {len(own)}; не найдено нигде: {missing}")

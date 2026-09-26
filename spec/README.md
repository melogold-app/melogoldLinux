# Векторы правил

Копии общих файлов правил: те же случаи прогоняют сервер, Android, Windows и этот клиент (тесты
`crates/melogold-core`). Случаи не правятся здесь — только копируются заново из источника
(docs/PROMPT.md §8).

| Файл | Правило | Источник |
|---|---|---|
| `youtube-links.vectors.json` | ссылки YouTube, REWRITE §4.9 | `melogoldAndroid/docs/spec`, коммит `733555d` |
| `title-cleaner.vectors.json` | очистка названий, REWRITE §4.10.8 | `melogoldAndroid/docs/spec`, коммит `733555d` |
| `server-address.vectors.json` | адрес сервера, API §7.1 | `melogoldAndroid/docs/spec`, коммит `733555d` |
| `lyrics.vectors.json`, `lyrics.md` | модель текстов, LRC и TTML | `melogoldAndroid/docs/spec`, коммит `733555d` |
| `import-ids.vectors.json` | id при импорте ViTune и ViMusic | `melogoldAndroid/docs/spec`, коммит `733555d` |
| `hwid.vectors.json` | идентификатор устройства, API §1.6 | `melogoldServer/spec`, коммит `78d1839` |
| `pow.vectors.json` | доказательство работы при регистрации, API §4.3 | `melogoldServer/spec`, коммит `78d1839` |
| `playlist-ops.vectors.json` | операции над плейлистами, API §4.8 | `melogoldServer/spec`, коммит `78d1839` |

Обновить: скопировать файлы из источника, поправить коммит в таблице, прогнать `cargo test --workspace`.

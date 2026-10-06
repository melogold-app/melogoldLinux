# Фикстуры InnerTube

Сырые ответы YouTube Music (`ytm/`, клиент WEB_REMIX) и YouTube (`web/`, клиент WEB) для тестов
разборщиков. Копии из `melogoldAndroid/providers/innertube/src/test/resources/fixtures`, коммит `33cdf95`:
там же `INDEX.md` — запросы, язык, дата записи и как ответы обезличены. Здесь не правятся —
только копируются заново из источника.

`search/` — выдача «Всё» для лучшего результата поиска (задание 0018): «Кино», «Michael Jackson»,
«Tkay Maidza» (исполнитель), «OK Computer» (альбом), «Bohemian Rhapsody» (трек). Копии из
`melogoldWindows/tests/Melogold.Tests/Fixtures/search`, коммит `7200c8c`; там они уже обезличены:
без `responseContext` (`visitorData`) и полей отслеживания.

`artist/` — страницы исполнителей «Кино» и «Michael Jackson» для шапки и «Об исполнителе» (задание 0019).
Копии из `melogoldWindows/tests/Melogold.Tests/Fixtures/artist`, коммит `b87dc07`, обезличены там же.

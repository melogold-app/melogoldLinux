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

`album-clips/` — альбом Tyler, The Creator «CALL ME IF YOU GET LOST» (`MPREb_rMqWqQe5N3L`) и его плейлист
(`VLOLAK5uy_np90iV2eD-YiScFePYgA7-i3c9kOYXMpc`): записаны 18.10.2026, WEB_REMIX `hl=en gl=US`, без входа. В
списке альбома все 16 строк — клипы (`MUSIC_VIDEO_TYPE_OMV`), в плейлисте — песни (`MUSIC_VIDEO_TYPE_ATV`):
LEMONHEAD в альбоме — клип `a5JCltuobqQ` на 1:25, в плейлисте — песня `QjckqsLmb1o` на 2:11. Обезличены
так же: без `responseContext` и полей отслеживания.

`album-clips/estate-sale.*` — переиздание «CALL ME IF YOU GET LOST: The Estate Sale» (`MPREb_VpXYEs5HeNs`), его плейлист
(`VLOLAK5uy_kSIbgi-WxAiBFs28kWN1cMy7bewO3-LCs`) и выдача поиска «Песни» по «Tyler, The Creator LEMONHEAD (feat. 42 Dugg)» и
«Tyler, The Creator CORSO»: записаны 20.10.2026, WEB_REMIX `hl=en gl=US`, без входа (задание 0033). Первые 16 строк
альбома — клипы, плейлист пуст (только `microformat`), песни есть только в поиске. Обезличены так же.

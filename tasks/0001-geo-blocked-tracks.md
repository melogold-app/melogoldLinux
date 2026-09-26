# Трек закрыт в стране: понятная ошибка со страной YouTube

Статус: открыто

Те же задания: `melogoldAndroid/tasks/0008-geo-blocked-tracks.md` (сделано, Android 0.1.8),
`melogoldWindows/tasks/0010-geo-blocked-tracks.md`, `melogoldiOSmacOS/tasks/0010-geo-blocked-tracks.md`.

## 1. Что нужно пользователю

Пользователь в России: Saba «Photosynthesis» не играет. С VPN через Хельсинки тоже: Google считает этот адрес
российским. Правообладатель открыл трек в 122 странах, России среди них нет, а клиенты показывали общую ошибку потока.

Пользователь: «нужно сделать логирование этой ошибки, чтобы человек понимал, что к чему, причём сразу на всех
платформах».

## 2. Решение (одинаково на всех клиентах)

Подробно, с проверенными ответами YouTube — `melogoldAndroid/tasks/0008-geo-blocked-tracks.md` (Android сделал это в 0.1.8).

Когда поток не получен, клиент **один раз** спрашивает YouTube, почему:

1. **Запрос:** `POST https://youtubei.googleapis.com/youtubei/v1/player`, клиент `WEB`
   (`clientName: WEB`, свежая `clientVersion` вида `2.2026…`), тело `{context, videoId}`, ответ не проверяется на
   пригодность. Таймаут 8 с. Клиенты потока (IOS, ANDROID_VR…) для этого не годятся: только `WEB`/`WEB_REMIX`
   присылают список стран, даже когда трек не играет.
2. **Из ответа:**
   - `playabilityStatus.status` и `.reason`;
   - `microformat.playerMicroformatRenderer.availableCountries` (у `WEB_REMIX` — `microformatDataRenderer`) — страны,
     где трек открыт;
   - `responseContext.visitorData` — страна, в которой YouTube видит устройство: base64 url-safe (`%3D` → `=`,
     `-` → `+`, `_` → `/`), это protobuf; поле 6 — вложенное сообщение, в его поле 1 — код страны из двух букв.
     Образцы: `CgtRTWpHWl9XellHZyjBhN7VBjIoCgJOTBIiEh4SHAsMDg8QERITFBUWFxgZGhscHR4fICEiIyQlJicgYw%3D%3D` → `NL`;
     `Cgs4bmZBZU9NZ2hGVSiLhd7VBjIOCgJERRII…` → `DE` (полная строка — в `VisitorDataTest.kt` Android). Разбор — простой
     обход полей protobuf (varint-ключ, тип 0/1/2/5), без библиотек.
3. **Классификация** (по порядку):
   - страна известна, список не пуст, страны в списке нет → **закрыт в стране** (страна + число стран);
   - в сообщении клиента потока или `reason` есть `available in your country`, `not made this video available in
     your country`, `blocked it in your country` → **закрыт в стране** (без числа);
   - `confirm your age`, `age-restricted`, `inappropriate for some users` → **возраст**;
   - `Private video`, `has been removed`, `account associated with this video has been terminated`,
     `no longer available` → **удалено или закрыто**;
   - иначе — прежняя общая ошибка.
4. **Текст в карточке ошибки** (рядом «Повторить · Пропустить · Другие версии»):

   | Случай | Русский | English |
   |---|---|---|
   | страна и число | Недоступно в стране «Россия»: YouTube считает, что вы там, а правообладатель открыл трек в 122 других странах. С VPN выберите сервер другой страны: некоторые серверы YouTube тоже относит к стране «Россия». | Unavailable in Russia: YouTube places you there, and the rights holder opened this track in 122 other countries. With a VPN, pick a server in another country: YouTube counts some VPN servers as Russia too. |
   | только страна | Недоступно в стране «Россия»: YouTube считает, что вы там, а правообладатель закрыл трек для этой страны. С VPN выберите сервер другой страны: некоторые серверы YouTube тоже относит к стране «Россия». | Unavailable in Russia: YouTube places you there, and the rights holder closed this track for it. With a VPN, pick a server in another country: YouTube counts some VPN servers as Russia too. |
   | без страны | Недоступно в вашей стране | Unavailable in your country |

   - Страна — полное название по коду на языке интерфейса («Россия», «Russia»); неизвестный код — сам код.
   - Число — с формами множественного числа: «в 121 другой стране», «в 122 других странах».
5. **Журнал:** одна строка на отказ — id трека, итог, статус и причина YouTube, страна, число стран, последнее
   сообщение клиента потока. Она попадает в журнал и в отчёт «Диагностики».


## 3. Linux

Кода клиента пока нет (стек выбирает пользователь, `docs/PROMPT.md` §3). Задание делается вместе со срезом
воспроизведения: ошибка «закрыт в стране» — со страной и числом стран, как в п. 2, текст ru/en с формами
множественного числа, строка в журнале.

## 4. Проверка

- **Юнит-тесты:**
  - страна из двух образцов `visitorData` (NL, DE) и `null` для мусора;
  - классификация: закрыт в стране (RU вне списка из 122) → страна и число; открыт → общая ошибка; фразы о стране,
    возрасте, удалении;
  - тексты на двух языках, формы 121/122.
- **Вручную:** Saba «Photosynthesis» (`cYKAr38pZcY`) с российского адреса (или через VPN, который YouTube считает
  российским) показывает «Недоступно в стране «Россия»… в 122 других странах…». Из другой страны трек играет как
  обычно.

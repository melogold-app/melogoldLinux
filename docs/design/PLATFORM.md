# Выжимка руководств: Linux

Ядро клиента по `docs/DESIGN-DOCTRINE.md` §2: GTK 4 и libadwaita (Rust), GNOME HIG.

Сверено: 2026-10-07 — живое состояние [GNOME HIG](https://developer.gnome.org/hig/) на эту дату и
[заметки GNOME 51](https://release.gnome.org/51/developers/index.html) (16.09.2026: GTK 4.23, libadwaita 1.10).
Проверялось на Fedora 44: GNOME 50.5, GTK 4.22, libadwaita 1.9.4. Минимум клиента — GTK 4.20 и libadwaita 1.8
(`docs/PROMPT.md` §3); Ubuntu 24.04, Debian 13 и Mint 22 получают Flatpak с runtime GNOME 49.

Каждый раздел: что говорит руководство (коротко, своими словами, со ссылкой на страницу) и как это сделано у нас.

## Компоненты по умолчанию

Руководство: стандартные виджеты и шаблоны — они уже умеют фокус, клавиатуру, доступность и адаптивность; своё — только
когда готового нет ([UI Styling](https://developer.gnome.org/hig/guidelines/ui-styling.html), «Custom Styling»).

| Элемент | Системный компонент | У нас |
|---|---|---|
| Окно и разделы | `AdwApplicationWindow`, `AdwNavigationSplitView` с боковой панелью (`.navigation-sidebar`), у раздела — `AdwNavigationView` | `window.rs` |
| Шапка | `AdwHeaderBar` в `AdwToolbarView`: «Назад» в начале, поиск по центру, главное меню в конце ([Header Bars]) | `window.rs` |
| Главное меню | `GtkMenuButton` `open-menu-symbolic`, «Главное меню»; в конце «Настройки», «Сочетания клавиш», «О программе» ([Menus]) | `window.rs::main_menu` |
| Поиск | `GtkSearchEntry`, Ctrl+F и «/» ([Search]) | `window.rs`, `pages/search.rs` |
| Переключатель выдачи и фильтров | `AdwToggleGroup` (1.7) | поиск «Всё · Музыка · YouTube», история, итоги |
| Список треков | `GtkListView` / `GtkListBox` со своей строкой | `track_row.rs`, `library_view.rs`, `catalog_widgets.rs` |
| Сетка карточек | `GtkFlowBox` (`GtkGridView` для длинных) ([Grid Views]) | `catalog_widgets::card_grid` |
| Полка | — системной нет | исключение «Полки» |
| Настройки | `AdwPreferencesPage`/`Group`, строки `AdwSwitchRow`, `AdwComboRow`, `AdwActionRow`, `AdwButtonRow` (1.6) ([Boxed Lists]) | `pages/settings.rs` |
| Пустое состояние, ошибка | `AdwStatusPage` с действием ([Placeholder Pages]) | `widgets.rs` |
| Загрузка | `AdwSpinner` (1.6), через 300 мс, по центру видимой области ([Spinners]) | `widgets.rs` |
| Событие с «Отменить» | `AdwToast` в `AdwToastOverlay` ([Toasts]) | `window.rs` |
| Длящееся состояние | `AdwBanner` ([Banners]) | нет сети, обновление |
| Подтверждение | `AdwAlertDialog`: отмена первой, Esc — отмена ([Dialogs]) | удаления, импорт |
| Лист, окно с содержимым | `AdwDialog` (на узком окне — снизу листом сам) | сведения, текст, пульт |
| Очередь | `AdwOverlaySplitView` справа | `queue_panel.rs` |
| Сочетания клавиш | `AdwShortcutsDialog` (1.8), Ctrl+? и F1 | `window.rs` |
| Аватар | `AdwAvatar` | аккаунт, устройства |

## Отступы и сетка

- Чисел HIG почти не даёт — их дают компоненты: поля строк, расстояния групп `AdwPreferencesPage`, отступы шапки.
- Содержимое широкого окна — в `AdwClamp` или в контейнерах с наибольшей шириной: длинные строки и далёкие элементы
  управления на большом экране читаются плохо ([Adaptive], «Large Size Handling»).
- Узкое окно: GNOME на телефоне — 360×294; у нас это минимум окна (`docs/PROMPT.md` §5.2), пороги — `AdwBreakpoint`.
- Скругления — libadwaita: элемент страницы (карточка, обложка) скругляется как `.card` (12), всплывающее и диалог —
  своими значениями темы. Одним числом на всё не скруглять (опыт Windows 2026-10-07).

## Типографика

- Шрифт — системный (Adwaita Sans, вариант Inter). Размеры в коде не задавать: классы `.title-1`…`.title-4`, `.heading`,
  `.body`, `.caption`, `.caption-heading`, `.numeric`; свои размеры — относительными величинами
  ([Typography]).
- Не писать заглавными и курсивом; меньше вариантов толщины и размера.
- Типографские знаки: «кавычки», «…», «×».

## Цвет и материалы

- Светлая и тёмная тема — от системы (`AdwStyleManager`); в настройках «Как в системе · Светлая · Тёмная»
  ([UI Styling]).
- Акцент — системный (`@accent_color`, `--accent-bg-color`); выбранный раздел боковой панели — цветом акцента.
- Своё оформление держать минимальным и строить из именованных цветов темы; каждое — в `EXCEPTIONS.md`.
- Высокая контрастность обязана работать: свои тени, отсветы и цвет обложки в ней выключены.
- Цвет — не единственный носитель смысла: у отметки ♡, «есть без сети» и ошибки — значок и подпись для Orca.

## Навигация

- Разделы — боковая панель (больше трёх равных видов, есть «Настройки»); в узком окне она сворачивается
  ([Navigation], [Sidebars]).
- Внутри раздела — стек страниц `AdwNavigationView`: «Назад» в шапке, Alt+←, жест; иерархия неглубокая.
- Esc: закрывает временное (подсказки поиска, выделение, диалог); на странице — «Назад», как задано в `docs/PROMPT.md`
  §5.3. В поле поиска Esc только снимает фокус/подсказки (§4.6 доктрины).
- Главное меню — в конце шапки, без «Выйти» ([Menus]).

## Значки

- Символические из темы Adwaita; свои — тем же стилем в `resources/icons` ([UI Icons]).
- В строках списков — символические без подложки ([Boxed Lists]); цветные плитки-значки Библиотеки — решение
  пользователя (`docs/PROMPT.md` §5.1), записано исключением.

## Движение

- Анимации — системные (`AdwTimedAnimation`, `AdwSpringAnimation` сами следуют `gtk-enable-animations`). Своё движение
  (столбики «играет», прокрутка текста) обязано останавливаться при выключенных анимациях и «Уменьшении движения»
  ([UI Styling], «Accessibility Considerations»).
- Ничего не мигает.

## Доступность

- У всего нажимаемого — доступное имя; у кнопок-значков — подсказка с сочетанием клавиш ([Header Bars]).
- Строки и карточки называют себя целиком («название, исполнитель»); порядок обхода сетки — по строкам.
- Проверка: Orca, высокая контрастность, крупный текст, справа налево не нужен (русский и английский).

## Клавиатура, мышь, жесты

- Стандарт GNOME: Ctrl+F — поиск, Ctrl+, — Настройки, Ctrl+? — сочетания, Ctrl+W — закрыть, Ctrl+Q — выйти, F10 —
  главное меню, Alt+← — назад ([Keyboard]).
- Свои — `docs/PROMPT.md` §5.3 (пробел, M, Ctrl+←/→ и т. д.).
- Меню у строк и карточек: правый щелчок, клавиша меню, Shift+F10 — одно и то же меню, что «…».
- Двойной щелчок и правый щелчок по кнопкам действий не назначаются ([Buttons]).

[Header Bars]: https://developer.gnome.org/hig/patterns/containers/header-bars.html
[Boxed Lists]: https://developer.gnome.org/hig/patterns/containers/boxed-lists.html
[Grid Views]: https://developer.gnome.org/hig/patterns/containers/grid-views.html
[Menus]: https://developer.gnome.org/hig/patterns/controls/menus.html
[Buttons]: https://developer.gnome.org/hig/patterns/controls/buttons.html
[Search]: https://developer.gnome.org/hig/patterns/nav/search.html
[Sidebars]: https://developer.gnome.org/hig/patterns/nav/sidebars.html
[Navigation]: https://developer.gnome.org/hig/guidelines/navigation.html
[Placeholder Pages]: https://developer.gnome.org/hig/patterns/feedback/placeholders.html
[Spinners]: https://developer.gnome.org/hig/patterns/feedback/spinners.html
[Toasts]: https://developer.gnome.org/hig/patterns/feedback/toasts.html
[Banners]: https://developer.gnome.org/hig/patterns/feedback/banners.html
[Dialogs]: https://developer.gnome.org/hig/patterns/feedback/dialogs.html
[Typography]: https://developer.gnome.org/hig/guidelines/typography.html
[UI Styling]: https://developer.gnome.org/hig/guidelines/ui-styling.html
[UI Icons]: https://developer.gnome.org/hig/guidelines/ui-icons.html
[Adaptive]: https://developer.gnome.org/hig/guidelines/adaptive.html
[Keyboard]: https://developer.gnome.org/hig/guidelines/keyboard.html

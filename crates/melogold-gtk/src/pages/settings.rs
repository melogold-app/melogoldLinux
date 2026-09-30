//! Настройки (docs/PROMPT.md §5.4, REWRITE §3.5). Только то, что есть на Android, плюс
//! платформенное; группы появляются вместе со срезом, который их оживляет.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use melogold_core::app_info::{ISSUES_URL, REPOSITORY_URL};
use melogold_core::settings::{keys, ThemeMode};
use melogold_server::account::AccountState;
use melogold_server::sync::SyncStatus;

use crate::app::{apply_theme, present_about};
use crate::localization::{tr, trf};
use crate::updates::UpdateState;
use crate::window::MainWindow;

const NEW_ISSUE_URL: &str = "https://github.com/melogold-app/melogoldLinux/issues/new";

pub fn root(window: &MainWindow) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.add(&account_group(window));
    page.add(&appearance_group(window));
    page.add(&playback_group(window));
    page.add(&library_group(window));
    page.add(&storage_group(window, &page));
    page.add(&database_group(window));
    page.add(&about_group(window));
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    content.append(&update_banner(window));
    page.set_vexpand(true);
    content.append(&page);
    adw::NavigationPage::builder().title(tr("NavSettings")).tag("root").child(&content).build()
}

/// Аккаунт и сервер (REWRITE §3.5.12–§3.5.13): «Работает без аккаунта» с «Войти» и «Создать
/// аккаунт», «Вы вошли как …» со статусом синхронизации, «Нужно войти снова»; сервер — адресом.
fn account_group(window: &MainWindow) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let account = adw::ActionRow::new();
    let icon = gtk::Image::from_icon_name("avatar-default-symbolic");
    account.add_prefix(&icon);
    let arrow = gtk::Image::from_icon_name("go-next-symbolic");
    account.add_suffix(&arrow);
    group.add(&account);
    // Без аккаунта — строки-кнопки под ним: в узком окне кнопки справа сжали бы текст.
    let sign_in = adw::ButtonRow::builder().title(tr("AccountSignIn")).build();
    sign_in.add_css_class("suggested-action");
    let register = adw::ButtonRow::builder().title(tr("AccountRegister")).build();
    group.add(&sign_in);
    group.add(&register);
    let server = adw::ActionRow::builder().title(tr("SettingsServer.Header")).activatable(true).build();
    server.add_prefix(&gtk::Image::from_icon_name("network-server-symbolic"));
    server.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    group.add(&server);

    let refresh: Rc<dyn Fn()> = {
        let (weak, account, icon, sign_in, register, arrow, server) =
            (window.downgrade(), account.clone(), icon.clone(), sign_in.clone(), register.clone(), arrow.clone(), server.clone());
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            let services = &window.ctx.services;
            let signed_in = matches!(services.account.state(), AccountState::SignedIn { .. });
            match services.account.state() {
                AccountState::SignedIn { login, .. } => {
                    account.set_title(&glib::markup_escape_text(&trf("AccountSignedInAsFormat", &[&login])));
                    let status = services.sync.status();
                    account.set_subtitle(&super::account::status_text(&status));
                    icon.set_icon_name(Some(if matches!(status, SyncStatus::Failed { .. }) {
                        "network-offline-symbolic"
                    } else {
                        "avatar-default-symbolic"
                    }));
                }
                AccountState::AuthRequired { .. } => {
                    account.set_title(tr("AccountAuthRequiredTitle"));
                    account.set_subtitle(tr("AccountAuthRequiredText"));
                    icon.set_icon_name(Some("dialog-warning-symbolic"));
                }
                AccountState::SignedOut => {
                    account.set_title(tr("NoAccountTitle"));
                    account.set_subtitle(tr("NoAccountText"));
                    icon.set_icon_name(Some("avatar-default-symbolic"));
                }
            }
            account.set_activatable(signed_in);
            arrow.set_visible(signed_in);
            sign_in.set_visible(!signed_in);
            register.set_visible(!signed_in);
            server.set_subtitle(&super::account::host(&services.account.server_url()));
        })
    };
    refresh();
    window.account_view.listen(&refresh);
    // Обработчик живёт, пока жива группа.
    let keep = RefCell::new(Some(refresh));
    group.connect_destroy(move |_| {
        keep.take();
    });
    let weak = window.downgrade();
    account.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.push(&super::account::account_page(&window));
        }
    });
    let weak = window.downgrade();
    sign_in.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.push(&super::account::sign_in_page(&window));
        }
    });
    let weak = window.downgrade();
    register.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.push(&super::account::register_page(&window));
        }
    });
    let weak = window.downgrade();
    server.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.push(&super::account::server_page(&window));
        }
    });
    group
}

fn appearance_group(window: &MainWindow) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("SettingsAppearance.Text")).build();
    let modes = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark];
    let names = gtk::StringList::new(&[tr("ThemeSystem.Content"), tr("ThemeLight.Content"), tr("ThemeDark.Content")]);
    let theme = adw::ComboRow::builder().title(tr("SettingsTheme.Header")).model(&names).build();
    let current = window.ctx.settings.get(&keys::THEME_MODE);
    theme.set_selected(modes.iter().position(|mode| *mode == current).unwrap_or(0) as u32);
    let settings = window.ctx.settings.clone();
    theme.connect_selected_notify(move |row| {
        let mode = modes.get(row.selected() as usize).copied().unwrap_or_default();
        settings.set(&keys::THEME_MODE, mode);
        apply_theme(mode);
    });
    group.add(&theme);

    // Названия языков — на своём языке (GLOSSARY: «Как в системе · Русский · English»).
    let locales = ["system", "ru", "en"];
    let names = gtk::StringList::new(&[tr("ThemeSystem.Content"), "Русский", "English"]);
    let language = adw::ComboRow::builder().title(tr("LinuxAppLanguage")).model(&names).build();
    let current = window.ctx.settings.get(&keys::APP_LOCALE);
    language.set_selected(locales.iter().position(|l| *l == current).unwrap_or(0) as u32);
    let settings = window.ctx.settings.clone();
    let weak = window.downgrade();
    language.connect_selected_notify(move |row| {
        let locale = locales.get(row.selected() as usize).copied().unwrap_or("system");
        settings.set(&keys::APP_LOCALE, locale.to_owned());
        if let Some(window) = weak.upgrade() {
            window.toast(tr("LinuxLanguageRestart"));
        }
    });
    group.add(&language);
    group
}

/// Скорость воспроизведения — одна на все треки, тон не меняется; нормализация громкости.
fn playback_group(window: &MainWindow) -> adw::PreferencesGroup {
    const SPEEDS: [f64; 7] = [0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];
    let group = adw::PreferencesGroup::builder().title(tr("SettingsPlayback.Text")).build();
    let ru = crate::localization::lang() == crate::localization::Lang::Ru;
    let labels: Vec<String> = SPEEDS
        .iter()
        .map(|speed| {
            if *speed == 1.0 {
                tr("SpeedNormal").to_owned()
            } else {
                let text = format!("{speed}×");
                if ru {
                    text.replace('.', ",")
                } else {
                    text
                }
            }
        })
        .collect();
    let names = gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    let speed = adw::ComboRow::builder().title(tr("SettingsSpeed.Header")).subtitle(tr("SettingsSpeed.Description")).model(&names).build();
    let current = window.ctx.settings.get(&keys::SPEED);
    let index =
        SPEEDS.iter().enumerate().min_by(|a, b| (a.1 - current).abs().total_cmp(&(b.1 - current).abs())).map(|(i, _)| i).unwrap_or(2);
    speed.set_selected(index as u32);
    let weak = window.downgrade();
    speed.connect_selected_notify(move |row| {
        let Some(window) = weak.upgrade() else { return };
        let value = SPEEDS.get(row.selected() as usize).copied().unwrap_or(1.0);
        window.ctx.settings.set(&keys::SPEED, value);
        window
            .ctx
            .services
            .player
            .send(melogold_playback::engine::Command::Settings(crate::services::playback_settings(&window.ctx.settings)));
    });
    group.add(&speed);

    let normalize = adw::SwitchRow::builder()
        .title(tr("SettingsNormalize.Header"))
        .subtitle(tr("SettingsNormalize.Description"))
        .active(window.ctx.settings.get(&keys::NORMALIZATION))
        .build();
    let weak = window.downgrade();
    normalize.connect_active_notify(move |row| {
        let Some(window) = weak.upgrade() else { return };
        window.ctx.settings.set(&keys::NORMALIZATION, row.is_active());
        window
            .ctx
            .services
            .player
            .send(melogold_playback::engine::Command::Settings(crate::services::playback_settings(&window.ctx.settings)));
    });
    group.add(&normalize);

    // «Управление с других устройств» (задание 0011): выключено — поток событий без `remote=1`.
    let remote = adw::SwitchRow::builder()
        .title(tr("LinuxRemoteAllow"))
        .subtitle(tr("LinuxRemoteAllowText"))
        .active(window.ctx.settings.get(&keys::REMOTE_CONTROL))
        .build();
    let weak = window.downgrade();
    remote.connect_active_notify(move |row| {
        let Some(window) = weak.upgrade() else { return };
        window.ctx.settings.set(&keys::REMOTE_CONTROL, row.is_active());
        window.ctx.services.sync.set_remote_control(row.is_active());
    });
    group.add(&remote);
    group
}

/// База данных (задание Windows 0004): «Сохранить копию» и «Импорт копии» — формат общий для всех клиентов.
fn database_group(window: &MainWindow) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("SettingsDatabase.Text")).build();
    let save =
        adw::ActionRow::builder().title(tr("SettingsBackup.Header")).subtitle(tr("SettingsBackup.Description")).activatable(true).build();
    save.add_prefix(&gtk::Image::from_icon_name("document-save-symbolic"));
    let weak = window.downgrade();
    save.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.save_backup();
        }
    });
    group.add(&save);
    let restore =
        adw::ActionRow::builder().title(tr("SettingsRestore.Header")).subtitle(tr("SettingsRestore.Description")).activatable(true).build();
    restore.add_prefix(&gtk::Image::from_icon_name("document-open-symbolic"));
    let weak = window.downgrade();
    restore.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.import_backup();
        }
    });
    group.add(&restore);
    group
}

/// «12,5 МБ», «1,2 ГБ», «340 КБ» — по языку приложения.
pub fn format_size(bytes: u64) -> String {
    let number = |value: f64| {
        let text = format!("{:.1}", value).trim_end_matches(".0").to_owned();
        if crate::localization::lang() == crate::localization::Lang::Ru {
            text.replace('.', ",")
        } else {
            text
        }
    };
    const MB: f64 = 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= 1024.0 * MB {
        trf("SizeGigabytesFormat", &[&number(value / 1024.0 / MB)])
    } else if value >= MB {
        trf("SizeMegabytesFormat", &[&number(value / MB)])
    } else {
        trf("SizeKilobytesFormat", &[&if bytes == 0 { 0 } else { (bytes / 1024).max(1) }])
    }
}

/// Строка с кнопкой справа («Очистить», «Сбросить»): кнопка гаснет, когда делать нечего.
fn button_row(title: &str, button: &str) -> (adw::ActionRow, gtk::Button) {
    let row = adw::ActionRow::builder().title(title).build();
    let action = gtk::Button::builder().label(button).valign(gtk::Align::Center).build();
    row.add_suffix(&action);
    (row, action)
}

/// Библиотека и история: приостановить историю прослушиваний и поиска, очистить поиск, сбросить скрытое.
fn library_group(window: &MainWindow) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("SettingsLibrary.Text")).build();
    let history = adw::SwitchRow::builder()
        .title(tr("SettingsPauseHistory.Header"))
        .subtitle(tr("SettingsPauseHistory.Description"))
        .active(window.ctx.settings.get(&keys::HISTORY_PAUSED))
        .build();
    let weak = window.downgrade();
    history.connect_active_notify(move |row| {
        let Some(window) = weak.upgrade() else { return };
        window.ctx.settings.set(&keys::HISTORY_PAUSED, row.is_active());
        window
            .ctx
            .services
            .player
            .send(melogold_playback::engine::Command::Settings(crate::services::playback_settings(&window.ctx.settings)));
    });
    group.add(&history);
    let search = adw::SwitchRow::builder()
        .title(tr("SettingsPauseSearch.Header"))
        .subtitle(tr("SettingsPauseSearch.Description"))
        .active(window.ctx.settings.get(&keys::SEARCH_HISTORY_PAUSED))
        .build();
    let settings = window.ctx.settings.clone();
    search.connect_active_notify(move |row| settings.set(&keys::SEARCH_HISTORY_PAUSED, row.is_active()));
    group.add(&search);
    let (searches, clear_searches) = button_row(tr("SettingsSearchHistory.Header"), tr("ClearSearchesButton.Content"));
    group.add(&searches);
    let (hidden, reset_hidden) = button_row(tr("SettingsHidden.Header"), tr("ResetHiddenButton.Content"));
    group.add(&hidden);

    let refresh: Rc<dyn Fn()> = {
        let (weak, searches, clear_searches, hidden, reset_hidden) =
            (window.downgrade(), searches.clone(), clear_searches.clone(), hidden.clone(), reset_hidden.clone());
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            let task = window.ctx.services.db(|library| {
                (library.recent_searches(1).map(|s| s.len()).unwrap_or(0), library.hidden_tracks().map(|h| h.len()).unwrap_or(0))
            });
            let (searches, clear_searches, hidden, reset_hidden) =
                (searches.clone(), clear_searches.clone(), hidden.clone(), reset_hidden.clone());
            glib::spawn_future_local(async move {
                let Some((search_count, hidden_count)) = task.await else { return };
                searches.set_subtitle(if search_count == 0 { tr("EmptySearchHistory") } else { "" });
                clear_searches.set_sensitive(search_count > 0);
                hidden.set_subtitle(&if hidden_count == 0 {
                    tr("BlacklistEmpty").to_owned()
                } else {
                    crate::localization::plural("Tracks", hidden_count as i64)
                });
                reset_hidden.set_sensitive(hidden_count > 0);
            });
        })
    };
    refresh();
    let run = |button: &gtk::Button, work: fn(&melogold_data::Library)| {
        let (weak, refresh) = (window.downgrade(), Rc::clone(&refresh));
        button.connect_clicked(move |_| {
            let Some(window) = weak.upgrade() else { return };
            let task = window.ctx.services.db(work);
            let refresh = Rc::clone(&refresh);
            glib::spawn_future_local(async move {
                let _ = task.await;
                refresh();
            });
        });
    };
    run(&clear_searches, |library| {
        let _ = library.clear_searches();
    });
    run(&reset_hidden, |library| {
        let _ = library.clear_hidden_tracks();
    });
    let keep = Rc::clone(&refresh);
    group.connect_map(move |_| keep());
    group
}

/// Хранилище (задание Windows 0009): кэш обложек и музыки с полосой заполнения и пределом,
/// загрузки отдельно (в кэш не входят и сами не удаляются), прочий кэш с найденными текстами.
fn storage_group(window: &MainWindow, page: &adw::PreferencesPage) -> adw::PreferencesGroup {
    const IMAGE_SIZES: [i64; 6] = [64, 128, 256, 512, 1024, 2048];
    const SONG_SIZES: [i64; 10] = [32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 0];
    let group =
        adw::PreferencesGroup::builder().title(tr("SettingsStorage.Text")).description(tr("SettingsStorageDescription.Text")).build();

    let size_names = |sizes: &[i64]| {
        let names: Vec<String> =
            sizes.iter().map(|mb| if *mb == 0 { tr("CacheUnlimited").to_owned() } else { format_size(*mb as u64 * 1024 * 1024) }).collect();
        gtk::StringList::new(&names.iter().map(String::as_str).collect::<Vec<_>>())
    };
    let cache_row = |title: &str, sizes: &[i64], current: i64, fallback: i64, clear: &str| {
        let row = adw::ExpanderRow::builder().title(title).build();
        // Полоса заполнения — в самой строке, а не внутри свёрнутой: занятость видна сразу
        // (задание Windows 0009). Без лимита полосы нет.
        let bar = gtk::LevelBar::builder().min_value(0.0).max_value(1.0).valign(gtk::Align::Center).width_request(96).build();
        bar.add_css_class("cache-level");
        row.add_suffix(&bar);
        let bar_row = bar.clone();
        let size = adw::ComboRow::builder().title(tr("SettingsImageCacheSize.Header")).model(&size_names(sizes)).build();
        let index = sizes.iter().position(|mb| *mb == current).or_else(|| sizes.iter().position(|mb| *mb == fallback)).unwrap_or(0);
        size.set_selected(index as u32);
        row.add_row(&size);
        let clear = adw::ButtonRow::builder().title(clear).build();
        row.add_row(&clear);
        group.add(&row);
        (row, bar, bar_row, size, clear)
    };
    let settings = &window.ctx.settings;
    let (images, images_bar, images_bar_row, images_size, images_clear) =
        cache_row(tr("SettingsImageCache.Header"), &IMAGE_SIZES, settings.get(&keys::IMAGE_CACHE_MB), 128, tr("ClearImagesButton.Content"));
    let (songs, songs_bar, songs_bar_row, songs_size, songs_clear) =
        cache_row(tr("SettingsSongCache.Header"), &SONG_SIZES, settings.get(&keys::STREAM_CACHE_MB), 4096, tr("ClearSongsButton.Content"));
    let (downloads, remove_downloads) = button_row(tr("SettingsDownloads.Header"), tr("RemoveDownloadsButton.Content"));
    remove_downloads.add_css_class("destructive-action");
    group.add(&downloads);
    let (other, clear_other) = button_row(tr("SettingsCache.Header"), tr("ClearCacheButton.Content"));
    group.add(&other);

    let refresh: Rc<dyn Fn()> = {
        let weak = window.downgrade();
        let widgets = (
            images.clone(),
            images_bar.clone(),
            images_bar_row.clone(),
            images_clear.clone(),
            songs.clone(),
            songs_bar.clone(),
            songs_bar_row.clone(),
            songs_clear.clone(),
            downloads.clone(),
            remove_downloads.clone(),
            other.clone(),
            clear_other.clone(),
        );
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            let services = &window.ctx.services;
            let (image_store, songs_store, download_store, library) =
                (services.images.clone(), Arc::clone(&services.songs), Arc::clone(&services.downloads), Arc::clone(&services.library));
            let (cache_dir, images_dir) = (window.ctx.paths.cache().to_path_buf(), window.ctx.paths.images());
            let images_size = async move { image_store.size().await };
            let task = services.run(async move {
                tokio::task::spawn_blocking(move || {
                    let other = dir_size(&cache_dir, &[images_dir.as_path()]) + library.fetched_lyrics_size().unwrap_or(0).max(0) as u64;
                    (songs_store.size(), download_store.size(), library.download_ids().map(|d| d.len()).unwrap_or(0), other)
                })
                .await
                .ok()
            });
            let widgets = widgets.clone();
            let settings = window.ctx.settings.clone();
            glib::spawn_future_local(async move {
                let image_bytes = images_size.await;
                let Some(Some((song_bytes, download_bytes, download_count, other_bytes))) = task.await else { return };
                let (
                    images,
                    images_bar,
                    images_bar_row,
                    images_clear,
                    songs,
                    songs_bar,
                    songs_bar_row,
                    songs_clear,
                    downloads,
                    remove,
                    other,
                    clear_other,
                ) = widgets;
                let image_max = settings.get(&keys::IMAGE_CACHE_MB).max(0) as u64 * 1024 * 1024;
                images.set_subtitle(&match (image_bytes * 100).checked_div(image_max) {
                    Some(percent) => trf("CacheUsedPercentFormat", &[&format_size(image_bytes), &percent.min(100)]),
                    None => trf("CacheUsedPlainFormat", &[&format_size(image_bytes)]),
                });
                images_bar_row.set_visible(image_max > 0);
                images_bar.set_value(if image_max > 0 { (image_bytes as f64 / image_max as f64).min(1.0) } else { 0.0 });
                images_clear.set_sensitive(image_bytes > 0);
                let song_max = settings.get(&keys::STREAM_CACHE_MB).max(0) as u64 * 1024 * 1024;
                songs.set_subtitle(&match (song_bytes * 100).checked_div(song_max) {
                    Some(percent) => trf("CacheUsedPercentFormat", &[&format_size(song_bytes), &percent.min(100)]),
                    None => trf("CacheUsedPlainFormat", &[&format_size(song_bytes)]),
                });
                songs_bar_row.set_visible(song_max > 0);
                songs_bar.set_value(if song_max > 0 { (song_bytes as f64 / song_max as f64).min(1.0) } else { 0.0 });
                songs_clear.set_sensitive(song_bytes > 0);
                downloads.set_subtitle(&if download_count == 0 {
                    tr("SettingsDownloadsEmpty").to_owned()
                } else {
                    format!("{} · {}", format_size(download_bytes), crate::localization::plural("Tracks", download_count as i64))
                });
                remove.set_sensitive(download_count > 0);
                other.set_subtitle(&trf("CacheUsedFormat", &[&format_size(other_bytes)]));
                clear_other.set_sensitive(other_bytes > 0);
            });
        })
    };
    refresh();
    // Кэш растёт, пока окно открыто: при каждом показе — заново.
    let keep = Rc::clone(&refresh);
    page.connect_map(move |_| keep());

    let later = |refresh: &Rc<dyn Fn()>| {
        let refresh = Rc::clone(refresh);
        move || {
            let refresh = Rc::clone(&refresh);
            glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || refresh());
        }
    };
    let (weak, again) = (window.downgrade(), later(&refresh));
    images_size.connect_selected_notify(move |row| {
        let Some(window) = weak.upgrade() else { return };
        let mb = IMAGE_SIZES.get(row.selected() as usize).copied().unwrap_or(128);
        window.ctx.settings.set(&keys::IMAGE_CACHE_MB, mb);
        window.ctx.services.images.set_max_bytes(mb * 1024 * 1024);
        again();
    });
    let (weak, again) = (window.downgrade(), later(&refresh));
    songs_size.connect_selected_notify(move |row| {
        let Some(window) = weak.upgrade() else { return };
        let mb = SONG_SIZES.get(row.selected() as usize).copied().unwrap_or(4096);
        window.ctx.settings.set(&keys::STREAM_CACHE_MB, mb);
        let songs = Arc::clone(&window.ctx.services.songs);
        let task = window.ctx.services.run(async move {
            tokio::task::spawn_blocking(move || {
                songs.set_max_bytes(if mb <= 0 { 0 } else { mb * 1024 * 1024 });
                songs.trim();
            })
            .await
        });
        let again = again.clone();
        glib::spawn_future_local(async move {
            let _ = task.await;
            again();
        });
    });
    let (weak, again) = (window.downgrade(), later(&refresh));
    images_clear.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.ctx.services.images.clear();
            again();
        }
    });
    let (weak, again) = (window.downgrade(), later(&refresh));
    songs_clear.connect_activated(move |_| {
        let Some(window) = weak.upgrade() else { return };
        let songs = Arc::clone(&window.ctx.services.songs);
        let task = window.ctx.services.run(async move { tokio::task::spawn_blocking(move || songs.clear()).await });
        let again = again.clone();
        glib::spawn_future_local(async move {
            let _ = task.await;
            again();
        });
    });
    let (weak, again) = (window.downgrade(), later(&refresh));
    clear_other.connect_clicked(move |_| {
        let Some(window) = weak.upgrade() else { return };
        let (cache_dir, images_dir, library) =
            (window.ctx.paths.cache().to_path_buf(), window.ctx.paths.images(), Arc::clone(&window.ctx.services.library));
        let task = window.ctx.services.run(async move {
            tokio::task::spawn_blocking(move || {
                clear_dir(&cache_dir, &[images_dir.as_path()]);
                let _ = library.clear_fetched_lyrics();
            })
            .await
        });
        let again = again.clone();
        glib::spawn_future_local(async move {
            let _ = task.await;
            again();
        });
    });
    let (weak, again) = (window.downgrade(), later(&refresh));
    remove_downloads.connect_clicked(move |_| {
        let Some(window) = weak.upgrade() else { return };
        confirm_remove_downloads(&window, again.clone());
    });
    group
}

/// «Удалить все загрузки?» — они единственная копия на устройстве; треки остаются в библиотеке, но без
/// сети не играют.
fn confirm_remove_downloads(window: &MainWindow, done: impl Fn() + Clone + 'static) {
    let (downloads, library) = (Arc::clone(&window.ctx.services.downloads), Arc::clone(&window.ctx.services.library));
    let task = window.ctx.services.run(async move {
        tokio::task::spawn_blocking(move || (downloads.size(), library.download_ids().map(|d| d.len()).unwrap_or(0))).await.ok()
    });
    let weak = window.downgrade();
    glib::spawn_future_local(async move {
        let (Some(window), Some(Some((bytes, count)))) = (weak.upgrade(), task.await) else { return };
        let dialog = adw::AlertDialog::new(
            Some(tr("SettingsDownloadsDeleteAllTitle")),
            Some(&trf(
                "SettingsDownloadsDeleteAllTextFormat",
                &[&crate::localization::plural("Tracks", count as i64), &format_size(bytes)],
            )),
        );
        dialog.add_response("cancel", tr("Cancel"));
        dialog.add_response("remove", tr("SettingsDownloadsDelete"));
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = window.downgrade();
        dialog.connect_response(Some("remove"), move |_, _| {
            let Some(window) = weak.upgrade() else { return };
            let downloads = Arc::clone(&window.ctx.services.downloads);
            let task = window.ctx.services.run(async move { tokio::task::spawn_blocking(move || downloads.remove_all()).await });
            let done = done.clone();
            glib::spawn_future_local(async move {
                let _ = task.await;
                done();
            });
        });
        dialog.present(Some(&window.window));
    });
}

/// Размер папки без `skip` (обложки — своя строка).
fn dir_size(dir: &std::path::Path, skip: &[&std::path::Path]) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .filter_map(Result::ok)
        .map(|entry| {
            let path = entry.path();
            if skip.iter().any(|s| path == *s) {
                0
            } else if path.is_dir() {
                dir_size(&path, skip)
            } else {
                entry.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

fn clear_dir(dir: &std::path::Path, skip: &[&std::path::Path]) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if skip.iter().any(|s| path == *s) {
            continue;
        }
        let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
    }
}

/// «Вышла новая версия X» вверху Настроек: «Обновить» — сначала «Что нового», если оно есть.
fn update_banner(window: &MainWindow) -> adw::Banner {
    let banner = adw::Banner::new("");
    let refresh: Rc<dyn Fn()> = {
        let (weak, banner) = (window.downgrade(), banner.clone());
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            let updates = &window.updates;
            let Some(manifest) = updates.available() else {
                banner.set_revealed(false);
                return;
            };
            let busy = matches!(updates.state(), UpdateState::Downloading(_) | UpdateState::Installing);
            let title = match updates.state() {
                UpdateState::Downloading(percent) => trf("UpdateDownloadingFormat", &[&percent]),
                UpdateState::Installing => tr("UpdateInstalling").to_owned(),
                UpdateState::Failed => tr("UpdateFailed").to_owned(),
                UpdateState::InstallFailed(key) => tr(key).to_owned(),
                _ => trf("UpdateAvailableTitle", &[&manifest.version]),
            };
            banner.set_title(&title);
            let label = if busy { None } else { Some(updates.action_label()) };
            banner.set_button_label(label);
            banner.set_revealed(true);
        })
    };
    refresh();
    window.updates.listen(&refresh);
    let keep = RefCell::new(Some(refresh));
    banner.connect_destroy(move |_| {
        keep.take();
    });
    let weak = window.downgrade();
    banner.connect_button_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            whats_new(&window);
        }
    });
    banner
}

/// «Что нового в X»: заметки релиза и «Обновить»; без заметок — сразу обновление.
/// Заметки выпуска пишутся Markdown-ом (они же — описание релиза на GitHub): пункты «- »
/// становятся «•», `код` — моноширинным, остальное — как есть.
fn notes_markup(notes: &str) -> String {
    notes
        .lines()
        .map(|line| {
            let (bullet, rest) = match line.trim_start().strip_prefix("- ").or_else(|| line.trim_start().strip_prefix("* ")) {
                Some(rest) => ("• ", rest),
                None => ("", line),
            };
            let mut out = String::from(bullet);
            for (index, part) in rest.split('`').enumerate() {
                let escaped = gtk::glib::markup_escape_text(part);
                if index % 2 == 1 {
                    out.push_str(&format!("<tt>{escaped}</tt>"));
                } else {
                    out.push_str(&escaped);
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn whats_new(window: &MainWindow) {
    let Some(manifest) = window.updates.available() else { return };
    let russian = crate::localization::lang() == crate::localization::Lang::Ru;
    let Some(notes) = manifest.notes_for(russian).map(str::to_owned) else {
        window.updates.install(Some(window.window.upcast_ref()));
        return;
    };
    let text = gtk::Label::builder()
        .label(notes_markup(&notes))
        .use_markup(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .build();
    // Без горизонтальной прокрутки: иначе высоту текста меряют по ширине в одну строку, и
    // от заметок оставалось две строки.
    let scroller = gtk::ScrolledWindow::builder()
        .child(&text)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .max_content_height(360)
        .propagate_natural_height(true)
        .build();
    let dialog = adw::AlertDialog::builder().heading(trf("UpdateWhatsNewTitle", &[&manifest.version])).extra_child(&scroller).build();
    dialog.add_response("later", tr("UpdateLater"));
    dialog.add_response("update", window.updates.action_label());
    dialog.set_response_appearance("update", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("update"));
    dialog.set_close_response("later");
    let weak = window.downgrade();
    dialog.connect_response(Some("update"), move |_, _| {
        if let Some(window) = weak.upgrade() {
            window.updates.install(Some(window.window.upcast_ref()));
        }
    });
    dialog.present(Some(&window.window));
}

/// «Лицензии»: Melogold и то, что едет вместе с ним; откуда звук, каталог и тексты.
fn present_licenses(window: &MainWindow) {
    let text = [
        "Melogold — GNU GPL 3.0",
        "GTK 4, libadwaita, GLib — GNU LGPL 2.1+",
        "GStreamer, gst-plugins-base, gst-libav (FFmpeg) — GNU LGPL 2.1+",
        "SQLite — public domain",
        "Rust, tokio, reqwest, rustls, serde, rusqlite, zbus, oo7, gtk-rs — MIT / Apache 2.0",
        tr("LicensesServices"),
    ]
    .join("\n\n");
    let label = gtk::Label::builder().label(&text).wrap(true).xalign(0.0).selectable(true).build();
    let scroller = gtk::ScrolledWindow::builder().child(&label).max_content_height(420).propagate_natural_height(true).build();
    let dialog = adw::AlertDialog::builder().heading(tr("LicensesTitle")).extra_child(&scroller).build();
    dialog.add_response("close", tr("Close"));
    dialog.present(Some(&window.window));
}

fn about_group(window: &MainWindow) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("SettingsAbout.Text")).build();

    // Версия и обновления: «Проверить обновления», пока новой версии нет; нашлась — полоса вверху.
    let version = adw::ActionRow::builder().title(tr("SettingsVersion.Header")).activatable(true).build();
    version.connect_activated(|row| present_about(row.root().and_downcast::<gtk::Window>().as_ref()));
    let check = gtk::Button::builder().label(tr("CheckUpdates")).valign(gtk::Align::Center).build();
    version.add_suffix(&check);
    let weak = window.downgrade();
    check.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            let found = window.downgrade();
            window.updates.check(
                true,
                Some(Rc::new(move |manifest: &melogold_core::updates::UpdateManifest| {
                    if let Some(window) = found.upgrade() {
                        window.updates.announce(manifest);
                    }
                })),
            );
        }
    });
    let refresh: Rc<dyn Fn()> = {
        let (weak, version, check) = (window.downgrade(), version.clone(), check.clone());
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            let updates = &window.updates;
            version.set_subtitle(&updates.version_text());
            let state = updates.state();
            check.set_visible(state != UpdateState::Disabled && updates.available().is_none());
            check.set_sensitive(state != UpdateState::Checking);
            check.set_label(if state == UpdateState::Checking { tr("UpdateChecking") } else { tr("CheckUpdates") });
        })
    };
    refresh();
    window.updates.listen(&refresh);
    let keep = RefCell::new(Some(refresh));
    group.connect_destroy(move |_| {
        keep.take();
    });
    group.add(&version);

    let shortcuts = adw::ActionRow::builder()
        .title(tr("SettingsShortcuts.Header"))
        .subtitle(tr("LinuxShortcutsDescription"))
        .activatable(true)
        .action_name("app.shortcuts")
        .build();
    group.add(&shortcuts);

    group.add(&super::link_row(tr("SettingsSource.Header"), tr("SettingsSource.Description"), REPOSITORY_URL));
    group.add(&super::link_row(tr("SettingsReportBug.Header"), tr("SettingsReportBug.Description"), ISSUES_URL));
    group.add(&super::link_row(tr("SettingsRequestFeature.Header"), tr("SettingsRequestFeature.Description"), NEW_ISSUE_URL));

    let diagnostics = super::next_row(tr("SettingsDiagnostics.Header"), tr("SettingsDiagnostics.Description"));
    let weak = window.downgrade();
    diagnostics.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.push(&super::diagnostics::page(&window));
        }
    });
    group.add(&diagnostics);

    let licenses = super::next_row(tr("SettingsLicenses.Header"), tr("SettingsLicenses.Description"));
    let weak = window.downgrade();
    licenses.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            present_licenses(&window);
        }
    });
    group.add(&licenses);
    group
}

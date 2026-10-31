//! «Диагностика» (docs/PROMPT.md §3 «Логи», REWRITE §3.5.10): версии, папка журналов, журнал одним файлом.
//! «Проверить извлечение» приходит со срезом воспроизведения.

use adw::prelude::*;
use gtk::{gio, glib};
use melogold_core::app_info::VERSION;

use crate::localization::tr;

/// Трек для проверки извлечения: стабильный, открыт во всех странах.
const TEST_VIDEO: &str = "dQw4w9WgXcQ";
use crate::window::MainWindow;

pub fn page(window: &MainWindow) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();

    let versions = adw::PreferencesGroup::builder().title(tr("LinuxVersions")).build();
    for (title, value) in version_rows() {
        let row = adw::ActionRow::builder().title(title).subtitle(value).subtitle_selectable(true).build();
        row.add_css_class("property");
        versions.add(&row);
    }
    // Где токены входа (docs/PROMPT.md §3): связки ключей нет — они в файле 0600, и это видно здесь.
    let account = &window.ctx.services.account;
    if account.session().is_some() {
        let in_keyring = account.store_kind() == melogold_server::session::StoreKind::Keyring;
        let row = adw::ActionRow::builder()
            .title(tr("LinuxTokens"))
            .subtitle(tr(if in_keyring { "LinuxTokensInKeyring" } else { "LinuxTokensInFile" }))
            .build();
        row.add_css_class("property");
        versions.add(&row);
    }
    page.add(&versions);

    // «Проверить извлечение» (REWRITE §3.5.10): время, itag и клиент или класс ошибки.
    let extraction = adw::PreferencesGroup::new();
    let check = adw::ActionRow::builder().title(tr("LinuxCheckExtraction")).subtitle(tr("LinuxCheckExtractionHint")).build();
    let run = gtk::Button::builder().label(tr("LinuxCheck")).valign(gtk::Align::Center).build();
    let weak = window.downgrade();
    let row = check.clone();
    run.connect_clicked(move |button| {
        let Some(window) = weak.upgrade() else { return };
        button.set_sensitive(false);
        row.set_subtitle(tr("UpdateChecking"));
        let resolver = window.ctx.services.resolver.clone();
        let task = window.ctx.services.run(async move {
            // Свежий адрес, а не из кэша: проверяется извлечение, а не память.
            resolver.invalidate(TEST_VIDEO);
            let started = std::time::Instant::now();
            let result = resolver.resolve(TEST_VIDEO).await;
            (started.elapsed(), result)
        });
        let (row, button) = (row.clone(), button.clone());
        glib::spawn_future_local(async move {
            let text = match task.await {
                Some((elapsed, Ok(info))) => format!("{} мс · itag {} · {}", elapsed.as_millis(), info.itag, info.source),
                Some((elapsed, Err(error))) => format!("{} мс · {:?} · {}", elapsed.as_millis(), error.kind, error.message),
                None => "—".into(),
            };
            row.set_subtitle(&text);
            button.set_sensitive(true);
        });
    });
    check.add_suffix(&run);
    extraction.add(&check);
    page.add(&extraction);

    let logs = adw::PreferencesGroup::builder().title(tr("LinuxLogs")).build();
    let folder = window.ctx.paths.logs();
    let row =
        adw::ActionRow::builder().title(tr("LinuxLogsFolder")).subtitle(folder.display().to_string()).subtitle_selectable(true).build();
    let open = gtk::Button::builder().label(tr("OpenLogs.Content")).valign(gtk::Align::Center).build();
    open.connect_clicked(move |button| {
        let window = button.root().and_downcast::<gtk::Window>();
        gtk::FileLauncher::new(Some(&gio::File::for_path(&folder))).launch(window.as_ref(), gio::Cancellable::NONE, |result| {
            if let Err(error) = result {
                tracing::warn!(%error, "папка журналов не открылась");
            }
        });
    });
    row.add_suffix(&open);
    logs.add(&row);
    logs.add(&save_log_row(window));
    page.add(&logs);

    adw::NavigationPage::builder().title(tr("Diagnostics")).tag("diagnostics").child(&page).build()
}

/// Версии приложения, GTK, libadwaita и система — на странице и в шапке журнала одним файлом.
fn version_rows() -> [(&'static str, String); 4] {
    let gtk_version = format!("{}.{}.{}", gtk::major_version(), gtk::minor_version(), gtk::micro_version());
    let adw_version = format!("{}.{}.{}", adw::major_version(), adw::minor_version(), adw::micro_version());
    [
        ("Melogold", VERSION.to_owned()),
        ("GTK", gtk_version),
        ("libadwaita", adw_version),
        (tr("LinuxSystem"), melogold_core::system::os_pretty_name()),
    ]
}

/// «Журнал одним файлом» (31.10.2026): шапка с версиями и временем, падение, прошлый запуск и этот —
/// один `.txt` туда, куда выберет пользователь, чтобы приложить к письму или сообщению.
fn save_log_row(window: &MainWindow) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(tr("LinuxLogsFile")).subtitle(tr("LinuxLogsFileHint")).build();
    let save = gtk::Button::builder().label(tr("LinuxSaveLog")).valign(gtk::Align::Center).build();
    let weak = window.downgrade();
    save.connect_clicked(move |_| {
        let Some(window) = weak.upgrade() else { return };
        let now = glib::DateTime::now_local().ok();
        let stamp = now.as_ref().and_then(|t| t.format("%Y%m%d-%H%M").ok()).map(|s| s.to_string()).unwrap_or_default();
        let dialog =
            gtk::FileDialog::builder().title(tr("LinuxLogsFile")).modal(true).initial_name(format!("melogold-log-{stamp}.txt")).build();
        if let Some(documents) = glib::user_special_dir(glib::UserDirectory::Documents) {
            dialog.set_initial_folder(Some(&gio::File::for_path(documents)));
        }
        let weak = window.downgrade();
        dialog.save(Some(&window.window), gio::Cancellable::NONE, move |result| {
            // Отмена выбора — не ошибка: ничего не делаем.
            let (Some(window), Ok(file)) = (weak.upgrade(), result) else { return };
            let Some(path) = file.path() else { return };
            let mut header: String = version_rows().iter().map(|(title, value)| format!("{title}: {value}\n")).collect();
            if let Some(time) = now.as_ref().and_then(|t| t.format_iso8601().ok()) {
                header.push_str(&format!("Время: {time}\n"));
            }
            let text = crate::logging::export(&window.ctx.paths.logs(), &header);
            match std::fs::write(&path, text) {
                Ok(()) => {
                    tracing::info!(путь = %path.display(), "журнал сохранён одним файлом");
                    let toast = adw::Toast::builder().title(tr("SaveFileDone")).button_label(tr("SaveFileOpen")).build();
                    let weak = window.downgrade();
                    toast.connect_button_clicked(move |_| {
                        let Some(window) = weak.upgrade() else { return };
                        gtk::FileLauncher::new(Some(&file)).launch(Some(&window.window), gio::Cancellable::NONE, |result| {
                            if let Err(error) = result {
                                tracing::warn!(%error, "файл журнала не открылся");
                            }
                        });
                    });
                    window.add_toast(toast);
                }
                Err(error) => {
                    tracing::warn!(%error, "журнал одним файлом не сохранился");
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    window.toast(&crate::localization::trf("SaveFileFailedFormat", &[&name]));
                }
            }
        });
    });
    row.add_suffix(&save);
    row
}

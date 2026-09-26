//! «Диагностика» (docs/PROMPT.md §3 «Логи», REWRITE §3.5.10): версии, папка журналов.
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
    let gtk_version = format!("{}.{}.{}", gtk::major_version(), gtk::minor_version(), gtk::micro_version());
    let adw_version = format!("{}.{}.{}", adw::major_version(), adw::minor_version(), adw::micro_version());
    for (title, value) in [
        ("Melogold", VERSION.to_owned()),
        ("GTK", gtk_version),
        ("libadwaita", adw_version),
        (tr("LinuxSystem"), melogold_core::system::os_pretty_name()),
    ] {
        let row = adw::ActionRow::builder().title(title).subtitle(value).subtitle_selectable(true).build();
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
    page.add(&logs);

    adw::NavigationPage::builder().title(tr("Diagnostics")).tag("diagnostics").child(&page).build()
}

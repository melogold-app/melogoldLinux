//! «Диагностика» (docs/PROMPT.md §3 «Логи», REWRITE §3.5.10): версии, папка журналов.
//! «Проверить извлечение» приходит со срезом воспроизведения.

use adw::prelude::*;
use gtk::gio;
use melogold_core::app_info::VERSION;

use crate::localization::tr;
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

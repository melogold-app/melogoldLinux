//! «Сохранить копию» и «Импорт копии» (задание Windows 0004, `backup-format.md`): выбор файла через
//! портал, ход импорта без кнопок, итог с числами и примечаниями, «Открыть историю».

use std::path::PathBuf;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use melogold_core::app_info::VERSION;
use melogold_data::backup::{self, ImportError, ImportSummary};
use melogold_server::account::AccountState;

use crate::localization::{tr, trf};
use crate::window::MainWindow;

impl MainWindow {
    /// «Сохранить копию»: файл `Melogold_backup_ггггММддЧЧммсс.db` — его открывает Melogold на любой платформе.
    pub fn save_backup(&self) {
        let stamp =
            glib::DateTime::now_local().ok().and_then(|now| now.format("%Y%m%d%H%M%S").ok()).map(|s| s.to_string()).unwrap_or_default();
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(tr("BackupFileType")));
        filter.add_pattern("*.db");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title(tr("SettingsBackup.Header"))
            .modal(true)
            .initial_name(backup::suggested_name(&stamp))
            .filters(&filters)
            .default_filter(&filter)
            .build();
        if let Some(documents) = glib::user_special_dir(glib::UserDirectory::Documents) {
            dialog.set_initial_folder(Some(&gio::File::for_path(documents)));
        }
        let weak = self.downgrade();
        dialog.save(Some(&self.window), gio::Cancellable::NONE, move |result| {
            let (Some(window), Ok(file)) = (weak.upgrade(), result) else { return };
            let Some(path) = file.path() else { return };
            let task = window.ctx.services.db(move |library| backup::export(library, &path, "linux", VERSION));
            glib::spawn_future_local(async move {
                match task.await {
                    Some(Ok(())) => window.toast(tr("BackupSaved")),
                    other => {
                        tracing::warn!(?other, "копия не сохранилась");
                        window.toast(tr("BackupFailed"));
                    }
                }
            });
        });
    }

    /// «Импорт копии»: копия ViTune, ViMusic или Melogold добавляется к библиотеке, ничего не удаляется.
    pub fn import_backup(&self) {
        let databases = gtk::FileFilter::new();
        databases.set_name(Some(tr("BackupFileType")));
        databases.add_pattern("*.db");
        databases.add_pattern("*.backup");
        // Копии приходят и без расширения.
        let all = gtk::FileFilter::new();
        all.set_name(Some("*"));
        all.add_pattern("*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&databases);
        filters.append(&all);
        let dialog =
            gtk::FileDialog::builder().title(tr("SettingsRestore.Header")).modal(true).filters(&filters).default_filter(&databases).build();
        if let Some(downloads) = glib::user_special_dir(glib::UserDirectory::Downloads) {
            dialog.set_initial_folder(Some(&gio::File::for_path(downloads)));
        }
        let weak = self.downgrade();
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |result| {
            let (Some(window), Ok(file)) = (weak.upgrade(), result) else { return };
            if let Some(path) = file.path() {
                window.run_import(path);
            }
        });
    }

    pub fn run_import(&self, path: PathBuf) {
        // Ход: «Импортируем библиотеку…» без кнопок, окно не подвисает.
        let spinner = adw::Spinner::builder().width_request(32).height_request(32).margin_top(12).build();
        let running = adw::AlertDialog::builder().heading(tr("ImportRunning")).extra_child(&spinner).can_close(false).build();
        running.present(Some(&self.window));
        let library = Arc::clone(&self.ctx.services.library);
        let task =
            self.ctx.services.run(async move { tokio::task::spawn_blocking(move || backup::import(&library, &path, VERSION)).await });
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let result = task.await.and_then(Result::ok);
            running.force_close();
            let Some(window) = weak.upgrade() else { return };
            match result {
                Some(Ok(summary)) => window.show_import_summary(&summary),
                Some(Err(error)) => window.show_import_failure(&error),
                None => window.show_import_failure(&ImportError::Unreadable(String::new())),
            }
        });
    }

    fn show_import_summary(&self, summary: &ImportSummary) {
        let mut text = trf(
            "ImportSummaryFormat",
            &[&summary.tracks, &summary.plays, &summary.favorites, &summary.lyrics, &summary.playlists, &summary.saved],
        );
        let mut note = |line: String| {
            text.push_str("\n\n");
            text.push_str(&line);
        };
        if summary.plays_known > 0 {
            note(trf("ImportPlaysKnownFormat", &[&summary.plays_known]));
        }
        if summary.local_skipped > 0 {
            note(trf("ImportLocalSkippedFormat", &[&summary.local_skipped]));
        }
        if summary.dates_skipped > 0 {
            note(trf("ImportDatesSkippedFormat", &[&summary.dates_skipped]));
        }
        if summary.plays > 0 && matches!(self.ctx.services.account.state(), AccountState::SignedIn { .. }) {
            note(tr("ImportSyncNote").to_owned());
        }
        tracing::info!(?summary, "импорт копии");
        let dialog = adw::AlertDialog::new(Some(tr("ImportDoneTitle")), Some(&text));
        dialog.add_response("history", tr("ImportOpenHistory"));
        dialog.add_response("done", tr("ImportDoneOk"));
        dialog.set_response_appearance("done", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("done"));
        dialog.set_close_response("done");
        let weak = self.downgrade();
        dialog.connect_response(Some("history"), move |_, _| {
            if let Some(window) = weak.upgrade() {
                window.show_tab(melogold_core::settings::Tab::Library);
                window.push(&crate::pages::library::history(&window));
            }
        });
        dialog.present(Some(&self.window));
    }

    fn show_import_failure(&self, error: &ImportError) {
        let reason = match error {
            ImportError::NotABackup => tr("ImportNotBackup"),
            ImportError::TooOld => tr("ImportTooOld"),
            ImportError::Unsupported => tr("ImportUnsupported"),
            ImportError::Unreadable(detail) => {
                tracing::warn!(%detail, "копия не читается");
                tr("ImportUnreadable")
            }
        };
        let dialog = adw::AlertDialog::new(Some(tr("ImportFailedTitle")), Some(reason));
        dialog.add_response("ok", tr("ImportDoneOk"));
        dialog.present(Some(&self.window));
    }
}

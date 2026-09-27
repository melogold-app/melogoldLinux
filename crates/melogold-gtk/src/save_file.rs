//! «Сохранить файлом» (Android `SaveFileEntry`, Windows `FileExport.cs`): трек — файлом .m4a с
//! названием, исполнителем, альбомом и обложкой туда, куда выберет пользователь (по умолчанию
//! «Музыка»). Байты — из загрузок или кэша, а если трека нет целиком нигде — из сети (заодно он
//! ляжет в кэш). Звук не перекодируется: те же кадры AAC ([`mp4_writer`]).

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use melogold_core::music::Track;
use melogold_core::thumbnails;
use melogold_playback::mp4_writer::{self, Tags};

use crate::localization::{tr, trf};
use crate::window::MainWindow;

const MAX_NAME_LENGTH: usize = 120;

impl MainWindow {
    pub fn save_file(&self, track: Track) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(tr("SaveFileType")));
        filter.add_suffix("m4a");
        filter.add_mime_type("audio/mp4");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title(tr("MenuSaveFile"))
            .modal(true)
            .initial_name(format!("{}.m4a", file_name(&self.display(&track))))
            .filters(&filters)
            .default_filter(&filter)
            .build();
        if let Some(music) = glib::user_special_dir(glib::UserDirectory::Music) {
            dialog.set_initial_folder(Some(&gio::File::for_path(music)));
        }
        let weak = self.downgrade();
        dialog.save(Some(&self.window), gio::Cancellable::NONE, move |result| {
            // Отмена выбора — не ошибка: ничего не делаем.
            let (Some(window), Ok(file)) = (weak.upgrade(), result) else { return };
            glib::spawn_future_local(async move { window.write_file(track, file).await });
        });
    }

    async fn write_file(&self, track: Track, file: gio::File) {
        let Some(path) = file.path() else { return };
        self.toast(&trf("SaveFileStartedFormat", &[&track.title]));
        let downloads = self.ctx.services.downloads.clone();
        let video_id = track.video_id.clone();
        let bytes = self.ctx.services.run(async move { downloads.read_whole(&video_id).await }).await;
        let cover = self.cover_bytes(&track).await;
        // Теги — со своими названиями пользователя (задание 0005).
        let shown = self.display(&track);
        let tags = Tags { title: Some(shown.title), artist: shown.artists_text, album: shown.album_title, cover };
        let written = match bytes {
            Some(Ok(bytes)) => {
                let target = path.clone();
                self.ctx
                    .services
                    .run(async move {
                        tokio::task::spawn_blocking(move || {
                            let m4a = mp4_writer::from_fragmented(&bytes, &tags).map_err(|e| e.to_string())?;
                            std::fs::write(&target, &m4a).map_err(|e| e.to_string())?;
                            Ok::<_, String>(m4a.len())
                        })
                        .await
                        .map_err(|e| e.to_string())?
                    })
                    .await
                    .unwrap_or_else(|| Err("отменено".into()))
            }
            Some(Err(error)) => Err(error.message),
            None => Err("отменено".into()),
        };
        match written {
            Ok(size) => {
                tracing::info!(трек = %track.video_id, кб = size / 1024, "сохранён файлом");
                let toast = adw::Toast::builder().title(tr("SaveFileDone")).button_label(tr("SaveFileOpen")).build();
                let weak = self.downgrade();
                toast.connect_button_clicked(move |_| {
                    let Some(window) = weak.upgrade() else { return };
                    gtk::FileLauncher::new(Some(&file)).launch(Some(&window.window), gio::Cancellable::NONE, |result| {
                        if let Err(error) = result {
                            tracing::warn!(%error, "файл не открылся");
                        }
                    });
                });
                self.add_toast(toast);
            }
            Err(error) => {
                tracing::warn!(трек = %track.video_id, %error, "сохранение файлом не удалось");
                self.toast(&trf("SaveFileFailedFormat", &[&track.title]));
            }
        }
    }

    /// Обложка 1200 px: JPEG и PNG — как есть, другое (WebP) — в PNG; нет — файл без обложки.
    async fn cover_bytes(&self, track: &Track) -> Option<Vec<u8>> {
        let fallback = thumbnails::for_video(&track.video_id, 1200);
        let url = thumbnails::sized(Some(track.thumbnail_url.as_deref().unwrap_or(&fallback)), 1200)?;
        let bytes = self.ctx.services.images.bytes(url).await?;
        if bytes.starts_with(&[0xFF, 0xD8]) || bytes.starts_with(&[0x89, 0x50]) {
            return Some(bytes);
        }
        let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()?;
        Some(texture.save_to_png_bytes().to_vec())
    }
}

/// «Исполнитель — Название» без знаков, которые файловые системы не пускают в имя.
pub fn file_name(track: &Track) -> String {
    let name = match track.artists_text.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        Some(artist) => format!("{artist} — {}", track.title),
        None => track.title.clone(),
    };
    let clean: String = name.chars().map(|c| if c.is_control() || r#"/\:*?"<>|"#.contains(c) { '_' } else { c }).collect();
    let clean = clean.trim().trim_end_matches('.');
    let cut: String = clean.chars().take(MAX_NAME_LENGTH).collect();
    let cut = cut.trim_end();
    if cut.is_empty() {
        track.video_id.clone()
    } else {
        cut.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_is_artist_and_title_without_bad_characters() {
        let track =
            Track { video_id: "abc".into(), title: "AC/DC: Live?".into(), artists_text: Some("AC/DC".into()), ..Default::default() };
        assert_eq!(file_name(&track), "AC_DC — AC_DC_ Live_");
        let bare = Track { video_id: "abc".into(), title: "...".into(), ..Default::default() };
        assert_eq!(file_name(&bare), "abc");
        let long = Track { video_id: "abc".into(), title: "я".repeat(300), ..Default::default() };
        assert_eq!(file_name(&long).chars().count(), MAX_NAME_LENGTH);
    }
}

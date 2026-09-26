//! Страницы разделов. Каждая — `AdwNavigationPage` в стеке своего раздела.

pub mod diagnostics;
pub mod search;
pub mod settings;

use adw::prelude::*;

/// Раздел, содержимое которого приходит в следующих срезах (docs/PROMPT.md §7).
pub fn placeholder(title: &str, icon: &str, description: &str) -> adw::NavigationPage {
    let status = adw::StatusPage::builder().icon_name(icon).title(title).description(description).build();
    adw::NavigationPage::builder().title(title).tag("root").child(&status).build()
}

/// Страница альбома, исполнителя, канала или плейлиста — со срезом «Каталог».
pub fn placeholder_for(item: &melogold_core::music::MusicItem) -> adw::NavigationPage {
    use melogold_core::music::MusicItem;
    let (title, icon) = match item {
        MusicItem::Album(a) => (a.title.clone(), "media-optical-cd-audio-symbolic"),
        MusicItem::Artist(a) => (a.name.clone(), "avatar-default-symbolic"),
        MusicItem::Playlist(p) => (p.title.clone(), "view-list-bullet-symbolic"),
        MusicItem::Mood(m) => (m.title.clone(), "view-grid-symbolic"),
        MusicItem::Track(t) => (t.title.clone(), "audio-x-generic-symbolic"),
    };
    let status =
        adw::StatusPage::builder().icon_name(icon).title(&title).description(crate::localization::tr("LinuxSectionSoonTrends")).build();
    adw::NavigationPage::builder().title(&title).child(&status).build()
}

/// Строка-ссылка наружу: открывает адрес в браузере.
pub fn link_row(title: &str, subtitle: &str, uri: &'static str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).subtitle(subtitle).activatable(true).build();
    row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    row.connect_activated(move |row| {
        let window = row.root().and_downcast::<gtk::Window>();
        gtk::UriLauncher::new(uri).launch(window.as_ref(), gtk::gio::Cancellable::NONE, |result| {
            if let Err(error) = result {
                tracing::warn!(%error, "ссылка не открылась");
            }
        });
    });
    row
}

/// Строка, которая открывает вложенную страницу.
pub fn next_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).subtitle(subtitle).activatable(true).build();
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    row
}

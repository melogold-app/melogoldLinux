//! Страницы разделов. Каждая — `AdwNavigationPage` в стеке своего раздела.

pub mod account;
pub mod catalog;
pub mod diagnostics;
pub mod library;
pub mod search;
pub mod settings;

use adw::prelude::*;

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

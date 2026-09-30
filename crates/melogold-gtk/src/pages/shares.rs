//! Ссылки на свои плейлисты (задание 0010, API §4.11): «Плейлист по ссылке» — снимок, открытый по
//! `melogold://share` или `/s/<код>` без входа, и «Мои ссылки» в аккаунте.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use melogold_core::iso;
use melogold_core::music::Track;
use melogold_playback::engine::Command;
use melogold_server::api::ApiError;
use melogold_server::dto::ShareDto;

use crate::catalog_widgets::{CollectionHeader, TrackContext, TrackList};
use crate::localization::{plural, tr};
use crate::widgets::StateView;
use crate::window::MainWindow;

/// Откуда снимок: с сервера по ссылке или уже готовый (снимки окна).
enum Source {
    Remote { base: String, id: String },
    Ready(Box<ShareDto>),
}

/// «Плейлист по ссылке»: название, треки, «Слушать», «Перемешать», «Сохранить в Библиотеку».
pub fn shared_playlist_page(window: &MainWindow, base: &str, id: &str) -> adw::NavigationPage {
    build(window, Source::Remote { base: base.to_owned(), id: id.to_owned() })
}

/// То же на готовом снимке, без сети (снимки окна).
pub fn shared_playlist_preview(window: &MainWindow, share: ShareDto) -> adw::NavigationPage {
    build(window, Source::Ready(Box::new(share)))
}

fn build(window: &MainWindow, source: Source) -> adw::NavigationPage {
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    let state = StateView::new(&content);
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    body.append(&state.root);
    let clamp = adw::Clamp::builder().maximum_size(1100).child(&body).build();
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();
    let page = adw::NavigationPage::builder().title(tr("LinuxSharedPlaylist")).child(&scroller).build();

    let source = Rc::new(source);
    let run: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::default();
    let again = Rc::clone(&run);
    let (weak, state_run) = (window.downgrade(), state.clone());
    let runner: Rc<dyn Fn()> = Rc::new(move || {
        let Some(window) = weak.upgrade() else { return };
        let (content, state, again, source) = (content.clone(), state_run.clone(), Rc::clone(&again), Rc::clone(&source));
        state.loading();
        let task = match &*source {
            Source::Remote { base, id } => {
                let (account, base, id) = (Arc::clone(&window.ctx.services.account), base.clone(), id.clone());
                Some(window.ctx.services.run(async move { account.open_share(&base, &id).await }))
            }
            Source::Ready(_) => None,
        };
        let weak = window.downgrade();
        glib::spawn_future_local(async move {
            let result: Option<Result<ShareDto, ApiError>> = match (&*source, task) {
                (Source::Ready(share), _) => Some(Ok((**share).clone())),
                (_, Some(task)) => task.await,
                _ => None,
            };
            let Some(window) = weak.upgrade() else { return };
            match result {
                Some(Ok(share)) => {
                    while let Some(child) = content.first_child() {
                        content.remove(&child);
                    }
                    show(&window, &content, &share);
                    state.content();
                }
                // Снимок удалён автором или ссылка неверна: повторять нечего.
                Some(Err(error)) if error.status == 404 => state.empty("dialog-warning-symbolic", tr("LinuxShareGone"), ""),
                Some(Err(error)) if error.is_network() => {
                    let again = again.borrow().clone();
                    state.error(tr("ErrorOffline"), move || {
                        if let Some(again) = &again {
                            again();
                        }
                    });
                }
                _ => {
                    let again = again.borrow().clone();
                    state.error(tr("ErrorUnknown"), move || {
                        if let Some(again) = &again {
                            again();
                        }
                    });
                }
            }
        });
    });
    run.replace(Some(Rc::clone(&runner)));
    runner();
    page
}

fn show(window: &MainWindow, content: &gtk::Box, share: &ShareDto) {
    let tracks: Vec<Track> = share.tracks.iter().map(|t| t.to_track()).collect();
    let cover = tracks.iter().find_map(|t| t.thumbnail_url.clone());
    let header = CollectionHeader::new(window, &share.name, "", Some(&plural("Tracks", tracks.len() as i64)), cover.as_deref(), false);
    let play = |shuffle: bool| {
        let (weak, tracks) = (window.downgrade(), tracks.clone());
        move || {
            if let Some(window) = weak.upgrade() {
                if !tracks.is_empty() {
                    window.ctx.services.player.send(Command::PlayList { tracks: tracks.clone(), start: 0, shuffle });
                }
            }
        }
    };
    header.add_button(tr("PlayAll"), "media-playback-start-symbolic", true, play(false));
    header.add_button(tr("Shuffle"), "media-playlist-shuffle-symbolic", false, play(true));
    // Сохранить — только явной кнопкой (API §7.2): свой плейлист с этими треками и их метаданными.
    let (weak, name, saving) = (window.downgrade(), share.name.clone(), tracks.clone());
    // Длинная подпись не влезает в ряд шапки: кнопка — под ней.
    let save = gtk::Button::builder()
        .child(&adw::ButtonContent::builder().label(tr("LinuxSaveToLibrary")).icon_name("list-add-symbolic").build())
        .halign(gtk::Align::Start)
        .margin_top(12)
        .build();
    save.add_css_class("pill");
    let save_button = save.clone();
    save.connect_clicked(move |button| {
        let Some(window) = weak.upgrade() else { return };
        button.set_sensitive(false);
        let (name, tracks) = (name.clone(), saving.clone());
        let task = window.ctx.services.db(move |library| library.create_playlist(&name, &tracks));
        let (weak, button) = (window.downgrade(), save_button.clone());
        glib::spawn_future_local(async move {
            let Some(window) = weak.upgrade() else { return };
            match task.await {
                Some(Ok(_)) => window.toast(tr("LinuxSavedToLibrary")),
                _ => button.set_sensitive(true),
            }
        });
    });
    content.append(&header.root);
    content.append(&save);
    let spacer = gtk::Box::builder().height_request(12).build();
    content.append(&spacer);
    content.append(&TrackList::new(window, &tracks, usize::MAX, TrackContext::List).list);
}

// ── «Мои ссылки» ──

/// Аккаунт › «Мои ссылки» (`GET /shares`): название, треков, дата; «Скопировать ссылку» и «Удалить».
pub fn my_links_page(window: &MainWindow) -> adw::NavigationPage {
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
    list.add_css_class("boxed-list");
    let intro = gtk::Label::builder().label(tr("LinuxMyLinksText")).xalign(0.0).wrap(true).margin_bottom(12).build();
    intro.add_css_class("dim-label");
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    content.append(&intro);
    content.append(&list);
    let state = StateView::new(&content);
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    let heading = gtk::Label::builder().label(tr("LinuxMyLinks")).xalign(0.0).margin_bottom(12).build();
    heading.add_css_class("title-1");
    body.append(&heading);
    body.append(&state.root);
    let clamp = adw::Clamp::builder().maximum_size(640).child(&body).build();
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();
    let page = adw::NavigationPage::builder().title(tr("LinuxMyLinks")).child(&scroller).build();

    let run: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::default();
    let again = Rc::clone(&run);
    let weak = window.downgrade();
    let runner: Rc<dyn Fn()> = Rc::new(move || {
        let Some(window) = weak.upgrade() else { return };
        state.loading();
        let account = Arc::clone(&window.ctx.services.account);
        let task = window.ctx.services.run(async move { account.shares().await });
        let (weak, list, state, again) = (window.downgrade(), list.clone(), state.clone(), Rc::clone(&again));
        glib::spawn_future_local(async move {
            let Some(window) = weak.upgrade() else { return };
            match task.await {
                Some(Ok(shares)) if shares.shares.is_empty() => {
                    state.empty("emblem-shared-symbolic", tr("LinuxMyLinksEmpty"), tr("LinuxMyLinksEmptyText"));
                }
                Some(Ok(shares)) => {
                    fill(&window, &list, shares.shares, &again);
                    state.content();
                }
                result => {
                    if let Some(Err(error)) = &result {
                        tracing::warn!(%error, "ссылки не загрузились");
                    }
                    let again = again.borrow().clone();
                    state.error(tr("ErrorOffline"), move || {
                        if let Some(again) = &again {
                            again();
                        }
                    });
                }
            }
        });
    });
    run.replace(Some(Rc::clone(&runner)));
    runner();
    page
}

fn fill(window: &MainWindow, list: &gtk::ListBox, shares: Vec<ShareDto>, reload: &Rc<RefCell<Option<Rc<dyn Fn()>>>>) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for share in shares {
        let date = iso::parse(&share.created_at).map(super::account::relative).unwrap_or_default();
        let subtitle =
            [plural("Tracks", share.tracks.len() as i64), date].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&share.name)).subtitle(subtitle).build();
        let copy =
            gtk::Button::builder().icon_name("edit-copy-symbolic").valign(gtk::Align::Center).tooltip_text(tr("LinuxCopyLink")).build();
        copy.add_css_class("flat");
        copy.update_property(&[gtk::accessible::Property::Label(tr("LinuxCopyLink"))]);
        let (weak, url) = (window.downgrade(), share.url.clone());
        copy.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                window.copy_link_text(&url, None);
            }
        });
        let delete =
            gtk::Button::builder().icon_name("user-trash-symbolic").valign(gtk::Align::Center).tooltip_text(tr("LinuxDeleteLink")).build();
        delete.add_css_class("flat");
        delete.update_property(&[gtk::accessible::Property::Label(tr("LinuxDeleteLink"))]);
        let (weak, id, reload) = (window.downgrade(), share.share_id.clone(), Rc::clone(reload));
        delete.connect_clicked(move |button| {
            let Some(window) = weak.upgrade() else { return };
            button.set_sensitive(false);
            let (account, id) = (Arc::clone(&window.ctx.services.account), id.clone());
            let task = window.ctx.services.run(async move { account.delete_share(&id).await });
            let (weak, button, reload) = (window.downgrade(), button.clone(), Rc::clone(&reload));
            glib::spawn_future_local(async move {
                let Some(window) = weak.upgrade() else { return };
                match task.await {
                    // Уже удалена — тоже хорошо: список читается заново.
                    Some(Ok(())) | Some(Err(ApiError { status: 404, .. })) => {
                        window.toast(tr("LinuxLinkDeleted"));
                        if let Some(reload) = reload.borrow().clone() {
                            reload();
                        }
                    }
                    _ => {
                        button.set_sensitive(true);
                        window.toast(tr("ErrorOffline"));
                    }
                }
            });
        });
        row.add_suffix(&copy);
        row.add_suffix(&delete);
        list.append(&row);
    }
}

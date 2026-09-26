//! Общие виджеты: обложка, строки выдачи, состояния экрана (docs/PROMPT.md §5.3, §5.5, §5.6).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use melogold_core::music::{MusicItem, Track};
use melogold_core::text::format_duration;
use melogold_core::thumbnails;

use crate::images::Images;
use crate::localization::tr;

// ── обложка ──

/// Обложка со скруглением; у видео — середина кадра квадратом (§5.5), без картинки — нота.
#[derive(Clone)]
pub struct Cover {
    pub root: gtk::Overlay,
    picture: gtk::Picture,
    placeholder: gtk::Image,
    requested: Rc<RefCell<Option<String>>>,
}

impl Cover {
    pub fn new(size: i32) -> Cover {
        let placeholder = gtk::Image::builder().icon_name("audio-x-generic-symbolic").pixel_size((size / 2).max(16)).build();
        placeholder.add_css_class("dim-label");
        let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Cover).can_shrink(true).build();
        let root = gtk::Overlay::builder().width_request(size).height_request(size).overflow(gtk::Overflow::Hidden).build();
        root.set_child(Some(&placeholder));
        root.add_overlay(&picture);
        root.add_css_class("cover");
        root.set_valign(gtk::Align::Center);
        root.set_halign(gtk::Align::Center);
        Cover { root, picture, placeholder, requested: Rc::default() }
    }

    /// Картинка по адресу нужного размера; пока грузится — нота.
    pub fn set(&self, images: &Images, url: Option<&str>, px: u32) {
        let url = thumbnails::sized(url, px);
        if *self.requested.borrow() == url && self.picture.paintable().is_some() {
            return;
        }
        self.requested.replace(url.clone());
        self.picture.set_paintable(gtk::gdk::Paintable::NONE);
        self.placeholder.set_visible(true);
        let Some(url) = url else { return };
        if let Some(texture) = images.cached(&url) {
            self.show(&texture);
            return;
        }
        let (this, images) = (self.clone(), images.clone());
        glib::spawn_future_local(async move {
            let texture = images.load(url.clone()).await;
            // Строку за это время могли отдать другому треку.
            if this.requested.borrow().as_deref() == Some(url.as_str()) {
                if let Some(texture) = texture {
                    this.show(&texture);
                }
            }
        });
    }

    fn show(&self, texture: &gtk::gdk::Texture) {
        self.picture.set_paintable(Some(texture));
        self.placeholder.set_visible(false);
    }
}

// ── строки ──

/// Строка трека (§5.3): обложка 40 px, название, под ним исполнитель и альбом; справа длительность и «…».
pub fn track_row(images: &Images, track: &Track, menu: Option<gtk::gio::MenuModel>) -> gtk::ListBoxRow {
    let cover = Cover::new(40);
    cover.set(images, track.thumbnail_url.as_deref().or(Some(&thumbnails::for_video(&track.video_id, 120))), 120);
    let title = gtk::Label::builder().label(&track.title).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    title.set_tooltip_text(Some(&track.title));
    let subtitle_text = match (&track.subtitle(), &track.views_text) {
        (s, Some(views)) if track.is_video() && !s.is_empty() => format!("{s} · {views}"),
        (s, _) => s.clone(),
    };
    let subtitle = gtk::Label::builder().label(&subtitle_text).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    subtitle.add_css_class("dim-label");
    subtitle.add_css_class("caption");
    let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).hexpand(true).spacing(2).build();
    texts.append(&title);
    if !subtitle_text.is_empty() {
        texts.append(&subtitle);
    }
    let content = gtk::Box::builder().spacing(12).margin_top(6).margin_bottom(6).margin_start(8).margin_end(4).build();
    content.append(&cover.root);
    content.append(&texts);
    if track.explicit {
        let badge = gtk::Label::builder().label("E").tooltip_text("Explicit").valign(gtk::Align::Center).build();
        badge.add_css_class("explicit-badge");
        content.append(&badge);
    }
    // Колонки постоянной ширины: значки стоят ровно, даже если в строке чего-то нет (§5.3).
    let duration_text = if track.is_live() { "LIVE".to_owned() } else { track.duration_ms.map(format_duration).unwrap_or_default() };
    let duration = gtk::Label::builder().label(&duration_text).width_chars(6).xalign(1.0).valign(gtk::Align::Center).build();
    duration.add_css_class("dim-label");
    duration.add_css_class("numeric");
    content.append(&duration);
    let more = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(tr("PlayerMore.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip"))
        .valign(gtk::Align::Center)
        .build();
    more.add_css_class("flat");
    if let Some(menu) = menu {
        more.set_menu_model(Some(&menu));
    } else {
        more.set_sensitive(false);
    }
    content.append(&more);
    let row = gtk::ListBoxRow::builder().child(&content).build();
    row.update_property(&[gtk::accessible::Property::Label(&format!("{}, {}", track.title, subtitle_text))]);
    if track.unavailable {
        row.add_css_class("dim-label");
    }
    row
}

/// Строка альбома, исполнителя, канала или плейлиста — той же раскладкой, что строка трека:
/// обложки и текст стоят в одну линию.
pub fn item_row(images: &Images, item: &MusicItem) -> Option<gtk::ListBoxRow> {
    let (title, subtitle, thumbnail, round) = match item {
        MusicItem::Album(a) => (a.title.clone(), a.subtitle(), a.thumbnail_url.clone(), false),
        MusicItem::Artist(a) => (a.name.clone(), a.subtitle.clone().unwrap_or_default(), a.thumbnail_url.clone(), true),
        MusicItem::Playlist(p) => (p.title.clone(), p.subtitle.clone().unwrap_or_default(), p.thumbnail_url.clone(), false),
        MusicItem::Mood(m) => (m.title.clone(), String::new(), None, false),
        MusicItem::Track(_) => return None,
    };
    let cover = Cover::new(40);
    if round {
        cover.root.add_css_class("round");
    }
    cover.set(images, thumbnail.as_deref(), 120);
    let title_label = gtk::Label::builder().label(&title).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    let subtitle_label = gtk::Label::builder().label(&subtitle).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    subtitle_label.add_css_class("dim-label");
    subtitle_label.add_css_class("caption");
    let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).hexpand(true).spacing(2).build();
    texts.append(&title_label);
    if !subtitle.is_empty() {
        texts.append(&subtitle_label);
    }
    let content = gtk::Box::builder().spacing(12).margin_top(6).margin_bottom(6).margin_start(8).margin_end(12).build();
    content.append(&cover.root);
    content.append(&texts);
    content.append(&gtk::Image::from_icon_name("go-next-symbolic"));
    let row = gtk::ListBoxRow::builder().child(&content).build();
    row.update_property(&[gtk::accessible::Property::Label(&format!("{title}, {subtitle}"))]);
    Some(row)
}

/// Заголовок группы в выдаче («YouTube Music», «YouTube»).
pub fn section_title(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(text).xalign(0.0).margin_top(18).margin_bottom(6).build();
    label.add_css_class("heading");
    label
}

// ── состояния ──

/// Четыре состояния экрана (§5.6): загрузка — крутилка через 300 мс, контент, пусто, ошибка с «Повторить».
#[derive(Clone)]
pub struct StateView {
    pub root: gtk::Stack,
    status: adw::StatusPage,
    retry: gtk::Button,
    token: Rc<RefCell<u64>>,
    on_retry: Rc<RefCell<Option<Box<dyn Fn()>>>>,
}

impl StateView {
    pub fn new(content: &impl IsA<gtk::Widget>) -> StateView {
        let root = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).vexpand(true).build();
        root.add_named(content, Some("content"));
        let spinner =
            adw::Spinner::builder().width_request(32).height_request(32).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
        root.add_named(&spinner, Some("loading"));
        root.add_named(&gtk::Box::new(gtk::Orientation::Vertical, 0), Some("blank"));
        let retry = gtk::Button::builder().label(tr("Retry")).halign(gtk::Align::Center).build();
        retry.add_css_class("pill");
        let status = adw::StatusPage::builder().child(&retry).vexpand(true).build();
        root.add_named(&status, Some("status"));
        let on_retry: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::default();
        let callback = Rc::clone(&on_retry);
        retry.connect_clicked(move |_| {
            if let Some(callback) = callback.borrow().as_ref() {
                callback();
            }
        });
        StateView { root, status, retry, token: Rc::default(), on_retry }
    }

    /// Загрузка: пусто, а через 300 мс — крутилка, чтобы быстрый ответ не мигал.
    pub fn loading(&self) {
        let token = self.next_token();
        self.root.set_visible_child_name("blank");
        let this = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            if *this.token.borrow() == token {
                this.root.set_visible_child_name("loading");
            }
        });
    }

    pub fn content(&self) {
        self.next_token();
        self.root.set_visible_child_name("content");
    }

    pub fn empty(&self, icon: &str, title: &str, description: &str) {
        self.next_token();
        self.status.set_icon_name(Some(icon));
        self.status.set_title(title);
        self.status.set_description(Some(description));
        self.retry.set_visible(false);
        self.root.set_visible_child_name("status");
    }

    pub fn error(&self, title: &str, retry: impl Fn() + 'static) {
        self.next_token();
        self.status.set_icon_name(Some("network-offline-symbolic"));
        self.status.set_title(title);
        self.status.set_description(None);
        self.retry.set_visible(true);
        self.on_retry.replace(Some(Box::new(retry)));
        self.root.set_visible_child_name("status");
    }

    fn next_token(&self) -> u64 {
        let mut token = self.token.borrow_mut();
        *token += 1;
        *token
    }
}

/// Текст ошибки каталога по классу (REWRITE §3.0).
pub fn catalog_error(kind: melogold_innertube::ErrorKind) -> &'static str {
    tr(match kind {
        melogold_innertube::ErrorKind::Offline => "ErrorOffline",
        melogold_innertube::ErrorKind::Blocked => "ErrorBlocked",
        melogold_innertube::ErrorKind::Parser => "ErrorParser",
        melogold_innertube::ErrorKind::Unknown => "ErrorUnknown",
    })
}

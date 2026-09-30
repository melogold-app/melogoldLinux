//! Строка трека (§5.3): обложка 40 px, название, исполнитель и альбом; справа колонки постоянной
//! ширины — ♡, «есть без сети», длительность, «…». Правый щелчок, клавиша меню и Shift+F10 — то же
//! меню, что «…»; по выделенному (от двух треков) — меню выделения (задание 0004).
//!
//! Строка переиспользуется: `GtkListView` отдаёт её другому треку ([`TrackRow::bind`]), поэтому ♡ и
//! метка обновляются по треку, который в строке сейчас, а не по тому, с которым её создали.

use std::cell::{Cell, OnceCell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};
use melogold_core::music::Track;
use melogold_core::text::format_duration;
use melogold_core::thumbnails;

use crate::library_view::{set_heart, show_mark, Offline, RowContext, TrackTarget};
use crate::localization::tr;
use crate::widgets::Cover;
use crate::window::{MainWindow, WeakWindow};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TrackRow {
        pub window: OnceCell<WeakWindow>,
        pub cover: OnceCell<Cover>,
        pub title: gtk::Label,
        pub subtitle: gtk::Label,
        pub badge: gtk::Label,
        pub heart: gtk::Button,
        pub mark: gtk::Stack,
        pub duration: gtk::Label,
        pub more: gtk::MenuButton,
        pub track: RefCell<Option<Track>>,
        pub context: Cell<RowContext>,
        /// Место в списке `GtkListView` (для выделения).
        pub position: Cell<u32>,
        /// Выделение списка, в котором строка: правый щелчок по выделенному открывает его меню.
        pub selection: RefCell<Option<std::rc::Weak<crate::selection::Selection>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TrackRow {
        const NAME: &'static str = "MelogoldTrackRow";
        type Type = super::TrackRow;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for TrackRow {}
    impl WidgetImpl for TrackRow {}
    impl BoxImpl for TrackRow {}
}

glib::wrapper! {
    pub struct TrackRow(ObjectSubclass<imp::TrackRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl TrackRow {
    pub fn new(window: &MainWindow) -> TrackRow {
        let row: TrackRow = glib::Object::builder().property("spacing", 12).build();
        row.add_css_class("track-row");
        let imp = row.imp();
        let _ = imp.window.set(window.downgrade());
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.set_margin_start(8);
        row.set_margin_end(4);

        let cover = Cover::new(40);
        row.append(&cover.root);
        let _ = imp.cover.set(cover);
        imp.title.set_xalign(0.0);
        imp.title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.subtitle.set_xalign(0.0);
        imp.subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.subtitle.add_css_class("dim-label");
        imp.subtitle.add_css_class("caption");
        let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).hexpand(true).spacing(2).build();
        texts.append(&imp.title);
        texts.append(&imp.subtitle);
        row.append(&texts);
        imp.badge.set_label("E");
        imp.badge.set_tooltip_text(Some("Explicit"));
        imp.badge.set_valign(gtk::Align::Center);
        imp.badge.add_css_class("explicit-badge");
        row.append(&imp.badge);

        // Колонки постоянной ширины: значки стоят ровно, даже если в строке чего-то нет.
        imp.heart.set_valign(gtk::Align::Center);
        imp.heart.set_tooltip_text(Some(tr("PlayerLike.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip")));
        imp.heart.add_css_class("flat");
        imp.heart.add_css_class("circular");
        imp.heart.add_css_class("heart");
        imp.heart.add_css_class("row-action");
        let weak = row.downgrade();
        imp.heart.connect_clicked(move |_| {
            let Some(row) = weak.upgrade() else { return };
            if let (Some(window), Some(track)) = (row.window(), row.track()) {
                // Состояние ведёт библиотека: set_liked перерисует все ♡ этого трека разом.
                let liked = !window.library_view.is_liked(&track.video_id);
                window.set_liked(vec![track], liked);
            }
        });
        row.append(&imp.heart);
        build_mark(&imp.mark);
        row.append(&imp.mark);
        imp.duration.set_width_chars(6);
        imp.duration.set_xalign(1.0);
        imp.duration.set_valign(gtk::Align::Center);
        imp.duration.add_css_class("dim-label");
        imp.duration.add_css_class("numeric");
        row.append(&imp.duration);
        imp.more.set_icon_name("view-more-symbolic");
        imp.more.set_tooltip_text(Some(tr("RowMenu")));
        imp.more.set_valign(gtk::Align::Center);
        imp.more.add_css_class("flat");
        imp.more.add_css_class("row-action");
        let weak = row.downgrade();
        imp.more.set_create_popup_func(move |button| {
            if let Some(row) = weak.upgrade() {
                if let Some(menu) = row.menu() {
                    button.set_menu_model(Some(&menu));
                }
            }
        });
        row.append(&imp.more);
        row.install_context_menu();
        window.library_view.register_row(&row);
        row
    }

    fn window(&self) -> Option<MainWindow> {
        self.imp().window.get().and_then(WeakWindow::upgrade)
    }

    pub fn track(&self) -> Option<Track> {
        self.imp().track.borrow().clone()
    }

    pub fn video_id(&self) -> Option<String> {
        self.imp().track.borrow().as_ref().map(|t| t.video_id.clone())
    }

    pub fn position(&self) -> u32 {
        self.imp().position.get()
    }

    pub fn set_position(&self, position: u32) {
        self.imp().position.set(position);
    }

    pub fn context(&self) -> RowContext {
        self.imp().context.get()
    }

    /// Delete: убрать из плейлиста, истории или очереди — тем же действием, что в меню.
    pub fn remove_from_place(&self) -> bool {
        let Some(track) = self.track() else { return false };
        let context = self.context();
        if matches!(context, RowContext::Plain | RowContext::Player) {
            return false;
        }
        let target = TrackTarget { track, context, ..Default::default() };
        self.activate_action("win.track-remove", Some(&target.variant())).is_ok()
    }

    pub fn set_selection(&self, selection: Option<&std::rc::Rc<crate::selection::Selection>>) {
        self.imp().selection.replace(selection.map(std::rc::Rc::downgrade));
    }

    /// Показать трек `track` из места `context` — со своими названиями пользователя (задание 0005);
    /// действия строки получают исходный трек.
    pub fn bind(&self, original: &Track, context: RowContext) {
        let imp = self.imp();
        let Some(window) = self.window() else { return };
        imp.context.set(context);
        let track = &window.display(original);
        let fallback = thumbnails::for_video(&track.video_id, 120);
        if let Some(cover) = imp.cover.get() {
            cover.set(&window.ctx.services.images, track.thumbnail_url.as_deref().or(Some(&fallback)), 120);
        }
        imp.title.set_label(&track.title);
        imp.title.set_tooltip_text(Some(&track.title));
        let subtitle = match (track.subtitle(), &track.views_text) {
            (s, Some(views)) if track.is_video() && !s.is_empty() => format!("{s} · {views}"),
            (s, _) => s,
        };
        imp.subtitle.set_label(&subtitle);
        imp.subtitle.set_visible(!subtitle.is_empty());
        imp.badge.set_visible(track.explicit);
        set_heart(&imp.heart, window.library_view.is_liked(&track.video_id));
        if let Some(cover) = imp.cover.get() {
            cover.set_playing(window.current_video_id().as_deref() == Some(track.video_id.as_str()));
        }
        show_mark(&imp.mark, window.offline_state(&track.video_id));
        imp.duration.set_label(&if track.is_live() {
            "LIVE".to_owned()
        } else {
            track.duration_ms.map(format_duration).unwrap_or_default()
        });
        self.update_property(&[gtk::accessible::Property::Label(&format!("{}, {subtitle}", track.title))]);
        if track.unavailable {
            self.add_css_class("dim-label");
        } else {
            self.remove_css_class("dim-label");
        }
        imp.track.replace(Some(original.clone()));
    }

    /// Показать заново тот же трек (правка названия, язык).
    pub fn rebind(&self) {
        if let Some(track) = self.track() {
            self.bind(&track, self.imp().context.get());
        }
    }

    /// Играет ли трек строки: столбики вместо обложки.
    pub fn refresh_playing(&self, current: Option<&str>) {
        if let Some(cover) = self.imp().cover.get() {
            cover.set_playing(current.is_some() && self.video_id().as_deref() == current);
        }
    }

    pub fn refresh_heart(&self, liked: bool) {
        set_heart(&self.imp().heart, liked);
    }

    pub fn refresh_mark(&self, state: Offline) {
        show_mark(&self.imp().mark, state);
    }

    /// Меню строки: выделено два трека и больше, и эта строка среди них — меню выделения, иначе — меню трека.
    fn menu(&self) -> Option<gtk::gio::Menu> {
        let window = self.window()?;
        let selection = self.imp().selection.borrow().as_ref().and_then(std::rc::Weak::upgrade);
        if let Some(selection) = selection.filter(|s| s.count() >= 2 && s.contains(self)) {
            window.show_selection(&selection);
            return Some(crate::selection::menu(&window));
        }
        let target = TrackTarget { track: self.track()?, context: self.imp().context.get(), ..Default::default() };
        Some(window.track_menu_for(&target))
    }

    /// Правый щелчок — то же меню, что «…». Клавиша меню и Shift+F10 ловятся списком: фокус — на его строке.
    fn install_context_menu(&self) {
        let click = gtk::GestureClick::builder().button(gdk::BUTTON_SECONDARY).build();
        let weak = self.downgrade();
        click.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(row) = weak.upgrade() {
                row.popup_menu(Some((x, y)));
            }
        });
        self.add_controller(click);
    }

    /// Меню строки у указателя или (с клавиатуры) у самой строки.
    pub fn popup_menu(&self, at: Option<(f64, f64)>) {
        if let Some(menu) = self.menu() {
            crate::library_view::popup(self.upcast_ref(), &menu, at);
        }
    }
}

fn build_mark(stack: &gtk::Stack) {
    stack.set_width_request(18);
    stack.set_valign(gtk::Align::Center);
    stack.add_named(&gtk::Box::new(gtk::Orientation::Horizontal, 0), Some("none"));
    let downloaded = gtk::Image::from_icon_name("offline-filled-symbolic");
    downloaded.add_css_class("accent");
    downloaded.set_tooltip_text(Some(tr("Downloads")));
    downloaded.update_property(&[gtk::accessible::Property::Label(tr("Downloads"))]);
    stack.add_named(&downloaded, Some("downloaded"));
    let cached = gtk::Image::from_icon_name("offline-outline-symbolic");
    cached.set_tooltip_text(Some(tr("LinuxInCache")));
    cached.update_property(&[gtk::accessible::Property::Label(tr("LinuxInCache"))]);
    stack.add_named(&cached, Some("cached"));
    let progress = gtk::DrawingArea::builder().content_width(16).content_height(16).build();
    progress.set_tooltip_text(Some(tr("MenuDownloadCancel")));
    stack.add_named(&progress, Some("downloading"));
    let failed = gtk::Image::from_icon_name("dialog-error-symbolic");
    failed.add_css_class("error");
    failed.set_tooltip_text(Some(tr("MenuDownloadRetry")));
    stack.add_named(&failed, Some("failed"));
}

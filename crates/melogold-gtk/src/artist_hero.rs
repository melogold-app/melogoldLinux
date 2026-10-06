//! Шапка исполнителя как в Apple Music и лист «Об исполнителе» (задание 0019, исключение «Страница
//! исполнителя» в `docs/design/EXCEPTIONS.md`; Windows `ArtistHero.cs`, `ArtistAboutDialog.cs`).
//!
//! Фото — широкий баннер YouTube Music (`musicImmersiveHeaderRenderer`, около 2,4 : 1) во всю ширину
//! страницы, внизу затемнение под текст; имя крупно поверх, под ним круглые кнопки поверх фото —
//! системный стиль `.osd` (HIG «Overlaid Controls»): «Об исполнителе», «Слушать» (крупная, акцентная),
//! «Подписаться», «…». Высота — по пропорциям фото, но не меньше 280 и не больше 60 % окна; длинное имя
//! переносится, а не режется «…». Затемнение одинаковое в обеих темах, поэтому текст поверх — всегда
//! светлый. При прокрутке, когда фото ушло под верх, сверху — полоса с именем и «Слушать».

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use melogold_core::music::ArtistDetails;
use melogold_core::thumbnails;

use crate::catalog_widgets::Toggle;
use crate::localization::{tr, trf};
use crate::window::MainWindow;

/// Низ и верх высоты шапки, доля окна.
const MIN_HEIGHT: f64 = 280.0;
const MAX_SHARE: f64 = 0.6;

pub struct ArtistHero {
    /// Шапка во всю ширину страницы.
    pub root: gtk::Overlay,
    /// Полоса с именем и «Слушать» поверх верха страницы — показывается, когда шапка ушла под верх.
    pub sticky: gtk::Revealer,
    /// «Подписаться» ↔ «Вы подписаны»: состояние ставит страница из базы.
    pub subscribe: Toggle,
    height: Rc<Cell<i32>>,
}

impl ArtistHero {
    /// Показать полосу, когда прокрутка `value` ушла за шапку.
    pub fn follow_scroll(&self, value: f64) {
        let threshold = f64::from(self.height.get()) - 56.0;
        self.sticky.set_reveal_child(self.height.get() > 0 && value > threshold);
    }
}

/// `play(shuffle)` — «Слушать» и «Перемешать»; `subscribed(on)` — нажатие «Подписаться»;
/// `copy_link` — «Копировать ссылку» из «…».
pub fn build(
    window: &MainWindow,
    artist: &ArtistDetails,
    play: Rc<dyn Fn(bool)>,
    subscribed: impl Fn(bool) + 'static,
    copy_link: impl Fn() + 'static,
) -> ArtistHero {
    let ratio = thumbnails::aspect(artist.thumbnail_url.as_deref()).unwrap_or(2.4);

    let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Cover).can_shrink(true).hexpand(true).build();
    picture.set_accessible_role(gtk::AccessibleRole::Presentation);
    let url = thumbnails::wide(artist.thumbnail_url.as_deref(), 1440);
    if let Some(url) = url {
        let (picture, images) = (picture.downgrade(), window.ctx.services.images.clone());
        glib::spawn_future_local(async move {
            if let (Some(texture), Some(picture)) = (images.load(url).await, picture.upgrade()) {
                picture.set_paintable(Some(&texture));
            }
        });
    }

    let shade = gtk::Box::builder().can_target(false).build();
    shade.add_css_class("artist-hero-shade");

    let name = gtk::Label::builder().label(&artist.name).xalign(0.0).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).build();
    name.add_css_class("artist-hero-name");
    name.set_accessible_role(gtk::AccessibleRole::Heading);

    let round = |icon: &str, tip: &str| {
        let button = gtk::Button::builder().icon_name(icon).tooltip_text(tip).valign(gtk::Align::Center).build();
        button.add_css_class("osd");
        button.add_css_class("circular");
        button.update_property(&[gtk::accessible::Property::Label(tip)]);
        button
    };
    // ⓘ — `help-about-symbolic`: в Adwaita 50 `dialog-information-symbolic` нарисован лампочкой.
    let about = round("help-about-symbolic", tr("ArtistAbout"));
    let listen = round("media-playback-start-symbolic", tr("PlayAll"));
    listen.remove_css_class("osd");
    listen.add_css_class("suggested-action");
    listen.add_css_class("artist-hero-play");

    let subscribe_button =
        gtk::ToggleButton::builder().icon_name("list-add-symbolic").tooltip_text(tr("Subscribe")).valign(gtk::Align::Center).build();
    subscribe_button.add_css_class("osd");
    subscribe_button.add_css_class("circular");
    subscribe_button.update_property(&[gtk::accessible::Property::Label(tr("Subscribe"))]);
    let subscribe = Toggle::new(subscribe_button.clone(), subscribed);
    subscribe_button.connect_toggled(|button| {
        let (icon, label) =
            if button.is_active() { ("object-select-symbolic", tr("Subscribed")) } else { ("list-add-symbolic", tr("Subscribe")) };
        button.set_icon_name(icon);
        button.set_tooltip_text(Some(label));
        button.update_property(&[gtk::accessible::Property::Label(label)]);
    });

    // «…»: «Перемешать» кнопкой не поместилось — первым пунктом, как у Windows.
    let actions = gio::SimpleActionGroup::new();
    let shuffle = gio::SimpleAction::new("shuffle", None);
    {
        let play = Rc::clone(&play);
        shuffle.connect_activate(move |_, _| play(true));
    }
    actions.add_action(&shuffle);
    let copy = gio::SimpleAction::new("copy-link", None);
    copy.connect_activate(move |_, _| copy_link());
    actions.add_action(&copy);
    let menu = gio::Menu::new();
    menu.append(Some(tr("Shuffle")), Some("artist.shuffle"));
    menu.append(Some(tr("LinuxCopyLink")), Some("artist.copy-link"));
    let more = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .tooltip_text(tr("MoreOptions"))
        .valign(gtk::Align::Center)
        .build();
    more.add_css_class("osd");
    more.add_css_class("circular");

    let buttons = gtk::Box::builder().spacing(12).build();
    buttons.append(&about);
    buttons.append(&listen);
    buttons.append(&subscribe_button);
    buttons.append(&more);

    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .valign(gtk::Align::End)
        .halign(gtk::Align::Fill)
        .margin_start(24)
        .margin_end(24)
        .margin_bottom(24)
        .build();
    info.append(&name);
    info.append(&buttons);

    let root = gtk::Overlay::builder().child(&picture).build();
    root.add_css_class("artist-hero");
    root.add_overlay(&shade);
    root.add_overlay(&info);
    root.insert_action_group("artist", Some(&actions));

    // Высота — по ширине: щуп во всю шапку сообщает её размер, а высота фото ставится из пропорций.
    let height = Rc::new(Cell::new(0));
    let probe = gtk::DrawingArea::builder().can_target(false).build();
    probe.set_accessible_role(gtk::AccessibleRole::Presentation);
    root.add_overlay(&probe);
    {
        let (picture, height, window) = (picture.downgrade(), Rc::clone(&height), window.window.downgrade());
        probe.connect_resize(move |_, width, _| {
            let Some(picture) = picture.upgrade() else { return };
            let window_height = window.upgrade().map(|w| w.height()).filter(|h| *h > 0).unwrap_or(760);
            let wanted = (f64::from(width) / ratio).clamp(MIN_HEIGHT, (f64::from(window_height) * MAX_SHARE).max(MIN_HEIGHT));
            let wanted = wanted.round() as i32;
            if wanted != height.get() {
                height.set(wanted);
                picture.set_height_request(wanted);
            }
        });
    }
    picture.set_height_request(MIN_HEIGHT as i32);

    {
        let (weak, artist) = (window.downgrade(), artist.clone());
        about.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                about_dialog(&window, &artist);
            }
        });
    }
    {
        let play = Rc::clone(&play);
        listen.connect_clicked(move |_| play(false));
    }

    // Полоса при прокрутке: имя и «Слушать» на системном фоне.
    let sticky_name = gtk::Label::builder().label(&artist.name).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build();
    sticky_name.add_css_class("heading");
    let sticky_play = gtk::Button::builder().icon_name("media-playback-start-symbolic").tooltip_text(tr("PlayAll")).build();
    sticky_play.add_css_class("circular");
    sticky_play.add_css_class("suggested-action");
    sticky_play.update_property(&[gtk::accessible::Property::Label(tr("PlayAll"))]);
    sticky_play.connect_clicked(move |_| play(false));
    let bar = gtk::Box::builder().spacing(12).margin_start(18).margin_end(18).margin_top(6).margin_bottom(6).build();
    bar.append(&sticky_name);
    bar.append(&sticky_play);
    let bar_surface = gtk::Box::builder().build();
    bar_surface.add_css_class("artist-sticky");
    bar_surface.append(&bar);
    bar.set_hexpand(true);
    let sticky = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .valign(gtk::Align::Start)
        .child(&bar_surface)
        .build();

    ArtistHero { root, sticky, subscribe, height }
}

/// «Об исполнителе» — системный лист: фото во всю ширину, имя и только то, что есть в данных YouTube
/// Music: слушатели в месяц, подписчики, просмотры, описание с источником. Жанра, даты рождения и
/// «откуда» в данных нет — их не выдумываем и пустыми не показываем.
pub fn about_dialog(window: &MainWindow, artist: &ArtistDetails) {
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_bottom(24).build();

    let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Cover).can_shrink(true).height_request(216).build();
    picture.set_accessible_role(gtk::AccessibleRole::Presentation);
    if let Some(url) = thumbnails::wide(artist.thumbnail_url.as_deref(), 1040) {
        let (picture, images) = (picture.downgrade(), window.ctx.services.images.clone());
        glib::spawn_future_local(async move {
            if let (Some(texture), Some(picture)) = (images.load(url).await, picture.upgrade()) {
                picture.set_paintable(Some(&texture));
            }
        });
    }
    column.append(&picture);

    let inner = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_start(24).margin_end(24).build();
    let name = gtk::Label::builder().label(&artist.name).xalign(0.0).wrap(true).build();
    name.add_css_class("title-1");
    inner.append(&name);

    // Числа YouTube пишет с неразрывными пробелами — их и оставляем.
    let facts: Vec<String> = [
        artist.monthly_listeners_text.clone(),
        artist.subscriber_count.as_deref().map(|count| trf("ArtistSubscribersFormat", &[&count])),
        artist.views_text.clone(),
    ]
    .into_iter()
    .flatten()
    .filter(|fact| !fact.trim().is_empty())
    .collect();
    if !facts.is_empty() {
        let line = gtk::Label::builder().label(facts.join(" · ")).xalign(0.0).wrap(true).build();
        line.add_css_class("dim-label");
        inner.append(&line);
    }

    let (body, source) = melogold_core::description::split(artist.description.as_deref());
    if !body.is_empty() {
        let text = gtk::Label::builder().label(&body).xalign(0.0).wrap(true).selectable(true).can_focus(false).build();
        text.add_css_class("body");
        inner.append(&text);
    }
    if let Some(footer) = crate::catalog_widgets::source_footer(source.as_ref()) {
        inner.append(&footer);
    }
    column.append(&inner);

    let scroller =
        gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).child(&column).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    let dialog = adw::Dialog::builder().title(tr("ArtistAbout")).content_width(560).child(&toolbar).build();
    dialog.present(Some(&window.window));
}

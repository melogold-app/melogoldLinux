//! «Сейчас играет» (docs/PROMPT.md §5.2): страница поверх окна, Esc и «Назад» закрывают — своей
//! «Свернуть» нет. Обложка песни — квадратом, кадр видео 16:9 — целиком прямоугольником. Текст
//! песни справа от обложки — со срезом «Тексты».

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use melogold_core::queue::RepeatMode;
use melogold_core::text::format_duration;
use melogold_core::thumbnails;
use melogold_playback::engine::{Command, State, Status};

use crate::localization::tr;
use crate::texts;
use crate::window::MainWindow;

#[derive(Clone)]
pub struct NowPlaying(Rc<Inner>);

pub struct Inner {
    pub page: adw::NavigationPage,
    frame: gtk::AspectFrame,
    picture: gtk::Picture,
    placeholder: gtk::Image,
    title: gtk::Label,
    subtitle: gtk::Label,
    error: gtk::Label,
    error_actions: gtk::Box,
    play: gtk::Button,
    play_icon: gtk::Image,
    previous: gtk::Button,
    next: gtk::Button,
    repeat: gtk::Button,
    adjustment: gtk::Adjustment,
    position: gtk::Label,
    duration: gtk::Label,
    requested: RefCell<Option<String>>,
    seeking: Cell<bool>,
    seek_token: Cell<u64>,
    updating: Cell<bool>,
}

impl std::ops::Deref for NowPlaying {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl NowPlaying {
    pub fn new(window: &MainWindow) -> NowPlaying {
        let placeholder = gtk::Image::builder().icon_name("audio-x-generic-symbolic").pixel_size(96).build();
        placeholder.add_css_class("dim-label");
        let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Cover).can_shrink(true).build();
        let overlay = gtk::Overlay::builder().overflow(gtk::Overflow::Hidden).build();
        overlay.set_child(Some(&placeholder));
        overlay.add_overlay(&picture);
        overlay.add_css_class("cover");
        overlay.add_css_class("large");
        let frame = gtk::AspectFrame::builder().ratio(1.0).obey_child(false).child(&overlay).vexpand(true).build();
        frame.set_size_request(200, 200);

        let title = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
        title.add_css_class("title-2");
        let subtitle = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
        subtitle.add_css_class("dim-label");
        let error = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).visible(false).build();
        error.add_css_class("error");
        // Рядом с причиной — «Повторить · Пропустить · Другие версии» (задание 0001).
        let error_actions = gtk::Box::builder().spacing(6).halign(gtk::Align::Center).visible(false).build();
        for (label, action) in [(tr("Retry"), "win.retry"), (tr("LinuxSkip"), "win.next"), (tr("MenuOtherVersions"), "win.other-versions")]
        {
            let button = gtk::Button::builder().label(label).action_name(action).build();
            button.add_css_class("pill");
            error_actions.append(&button);
        }

        let adjustment = gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 5.0, 0.0);
        let scale = gtk::Scale::builder().adjustment(&adjustment).hexpand(true).draw_value(false).build();
        scale.update_property(&[gtk::accessible::Property::Label(tr(
            "PlayerSeek.[using:Microsoft.UI.Xaml.Automation]AutomationProperties.Name",
        ))]);
        let position = gtk::Label::builder().label("0:00").width_chars(5).build();
        let duration = gtk::Label::builder().label("0:00").width_chars(5).build();
        for label in [&position, &duration] {
            label.add_css_class("numeric");
            label.add_css_class("dim-label");
        }
        let seek = gtk::Box::builder().spacing(8).build();
        seek.append(&position);
        seek.append(&scale);
        seek.append(&duration);

        let button = |icon: &str, tooltip: &str, action: &str| {
            let button =
                gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).action_name(action).valign(gtk::Align::Center).build();
            button.add_css_class("flat");
            button.add_css_class("circular");
            button
        };
        let shuffle = gtk::ToggleButton::builder()
            .icon_name("media-playlist-shuffle-symbolic")
            .tooltip_text(tr("PlayerShuffle.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip"))
            .action_name("win.shuffle")
            .valign(gtk::Align::Center)
            .build();
        shuffle.add_css_class("flat");
        shuffle.add_css_class("circular");
        let previous = button(
            "media-skip-backward-symbolic",
            tr("PlayerPrevious.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip"),
            "win.previous",
        );
        let play_icon = gtk::Image::builder().icon_name("media-playback-start-symbolic").pixel_size(24).build();
        let play =
            gtk::Button::builder().child(&play_icon).action_name("win.play-pause").tooltip_text(format!("{} (Space)", tr("Play"))).build();
        play.add_css_class("circular");
        play.add_css_class("suggested-action");
        play.add_css_class("now-playing-play");
        let next =
            button("media-skip-forward-symbolic", tr("PlayerNext.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip"), "win.next");
        let repeat = button("media-playlist-consecutive-symbolic", tr("RepeatOff"), "win.repeat");
        let controls = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).build();
        for widget in
            [shuffle.upcast_ref::<gtk::Widget>(), previous.upcast_ref(), play.upcast_ref(), next.upcast_ref(), repeat.upcast_ref()]
        {
            controls.append(widget);
        }

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        column.append(&frame);
        column.append(&title);
        column.append(&subtitle);
        column.append(&error);
        column.append(&error_actions);
        column.append(&seek);
        column.append(&controls);
        let clamp = adw::Clamp::builder().maximum_size(560).child(&column).build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&clamp));
        let page = adw::NavigationPage::builder().title(tr("NowPlaying")).tag("now-playing").child(&toolbar).build();

        let now_playing = NowPlaying(Rc::new(Inner {
            page,
            frame,
            picture,
            placeholder,
            title,
            subtitle,
            error,
            error_actions,
            play,
            play_icon,
            previous,
            next,
            repeat,
            adjustment,
            position,
            duration,
            requested: RefCell::default(),
            seeking: Cell::new(false),
            seek_token: Cell::new(0),
            updating: Cell::new(false),
        }));

        let (weak, player) = (Rc::downgrade(&now_playing.0), window.ctx.services.player.clone());
        now_playing.adjustment.connect_value_changed(move |adjustment| {
            let Some(inner) = weak.upgrade() else { return };
            if inner.updating.get() {
                return;
            }
            inner.seeking.set(true);
            inner.position.set_label(&format_duration((adjustment.value() * 1000.0) as i64));
            let token = inner.seek_token.get() + 1;
            inner.seek_token.set(token);
            let (weak, player) = (Rc::downgrade(&inner), player.clone());
            glib::timeout_add_local_once(Duration::from_millis(150), move || {
                if let Some(inner) = weak.upgrade() {
                    if inner.seek_token.get() == token {
                        player.send(Command::Seek(Duration::from_secs_f64(inner.adjustment.value())));
                        inner.seeking.set(false);
                    }
                }
            });
        });
        now_playing
    }

    pub fn apply(&self, window: &MainWindow, state: &State) {
        if let Some(track) = &state.track {
            self.title.set_label(&track.title);
            self.subtitle.set_label(&track.subtitle());
            // Кадр видео 16:9 — целиком прямоугольником, обложка песни — квадратом (§5.2, §5.5).
            let wide = thumbnails::is_wide(track.thumbnail_url.as_deref()) || (track.thumbnail_url.is_none() && track.is_video());
            self.frame.set_ratio(if wide { 16.0 / 9.0 } else { 1.0 });
            let source = track.thumbnail_url.clone().unwrap_or_else(|| thumbnails::for_video(&track.video_id, 544));
            let url = thumbnails::sized(Some(&source), 544);
            if *self.requested.borrow() != url {
                self.requested.replace(url.clone());
                self.picture.set_paintable(gtk::gdk::Paintable::NONE);
                self.placeholder.set_visible(true);
                if let Some(url) = url {
                    let (this, images) = (self.clone(), window.ctx.services.images.clone());
                    glib::spawn_future_local(async move {
                        let texture = images.load(url.clone()).await;
                        if this.requested.borrow().as_deref() == Some(url.as_str()) {
                            if let Some(texture) = texture {
                                this.picture.set_paintable(Some(&texture));
                                this.placeholder.set_visible(false);
                            }
                        }
                    });
                }
            }
        }
        match &state.error {
            Some(error) if state.status == Status::Error => {
                self.error.set_label(&texts::error_text(error));
                self.error.set_visible(true);
                self.error_actions.set_visible(true);
            }
            _ => {
                self.error.set_visible(false);
                self.error_actions.set_visible(false);
            }
        }
        self.play_icon.set_icon_name(Some(if state.playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" }));
        self.play.set_tooltip_text(Some(&format!("{} (Space)", tr(if state.playing { "Pause" } else { "Play" }))));
        self.next.set_sensitive(state.has_next);
        self.previous.set_sensitive(state.has_previous);
        let (icon, key) = match state.repeat {
            RepeatMode::Off => ("media-playlist-consecutive-symbolic", "RepeatOff"),
            RepeatMode::All => ("media-playlist-repeat-symbolic", "RepeatAll"),
            RepeatMode::One => ("media-playlist-repeat-song-symbolic", "RepeatOne"),
        };
        self.repeat.set_icon_name(icon);
        self.repeat.set_tooltip_text(Some(&format!("{} (Ctrl+T)", tr(key))));
        let duration = state.duration.unwrap_or_default();
        self.updating.set(true);
        self.adjustment.set_upper(duration.as_secs_f64().max(1.0));
        self.updating.set(false);
        self.duration.set_label(&format_duration(duration.as_millis() as i64));
    }

    pub fn tick(&self, position: Option<Duration>) {
        if self.seeking.get() {
            return;
        }
        let position = position.unwrap_or_default();
        self.updating.set(true);
        self.adjustment.set_value(position.as_secs_f64().min(self.adjustment.upper()));
        self.updating.set(false);
        self.position.set_label(&format_duration(position.as_millis() as i64));
    }
}

//! Панель воспроизведения внизу во всю ширину (docs/PROMPT.md §5.2).
//!
//! ```text
//! [обложка][название / исполнитель]   [⇄ ⏮ ⏯ ⏭ ⟲ / ——●—— время]   [Очередь][громкость][…]
//! ```
//! Боковые колонки — у `GtkCenterBox`, поэтому ⏯ стоит посередине окна. Пороги ширины (`AdwBreakpoint`
//! в окне): от 1100 — всё; уже — громкость кнопкой с поповером; от 720 и уже — панель в две строки:
//! сверху перемотка во всю ширину, снизу трек и кнопки; перемешать и повтор уходят в «…».

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};
use melogold_core::queue::RepeatMode;
use melogold_core::settings::keys;
use melogold_core::text::format_duration;
use melogold_playback::engine::{Command, State, Status};

use crate::localization::tr;
use crate::texts;
use crate::widgets::Cover;
use crate::window::MainWindow;

#[derive(Clone)]
pub struct PlayerBar(Rc<Inner>);

pub struct Inner {
    pub root: gtk::Box,
    cover: Cover,
    title: gtk::Label,
    subtitle: gtk::Label,
    error_box: gtk::Box,
    error_label: gtk::Label,
    pub shuffle: gtk::ToggleButton,
    previous: gtk::Button,
    play: gtk::Button,
    play_stack: gtk::Stack,
    next: gtk::Button,
    pub repeat: gtk::Button,
    adjustment: gtk::Adjustment,
    pub seek_center: gtk::Box,
    pub seek_top: gtk::Box,
    position_labels: [gtk::Label; 2],
    duration_labels: [gtk::Label; 2],
    pub queue: gtk::ToggleButton,
    pub volume_inline: gtk::Box,
    pub volume_button: gtk::MenuButton,
    volume: gtk::Adjustment,
    mute_buttons: [gtk::ToggleButton; 2],
    volume_value: gtk::Label,
    more: gtk::MenuButton,
    state: RefCell<State>,
    /// Пользователь тянет ползунок: позиция из плеера его не перебивает.
    seeking: Cell<bool>,
    seek_token: Cell<u64>,
    resolving_since: Cell<Option<Instant>>,
    updating: Cell<bool>,
}

impl std::ops::Deref for PlayerBar {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

fn icon_button(icon: &str, tooltip: &str, name: &str) -> gtk::Button {
    let button = gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).valign(gtk::Align::Center).build();
    button.update_property(&[gtk::accessible::Property::Label(name)]);
    button.add_css_class("flat");
    button
}

fn toggle_button(icon: &str, tooltip: &str, name: &str) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::builder().icon_name(icon).tooltip_text(tooltip).valign(gtk::Align::Center).build();
    button.update_property(&[gtk::accessible::Property::Label(name)]);
    button.add_css_class("flat");
    button
}

fn time_label() -> gtk::Label {
    let label = gtk::Label::builder().label("0:00").width_chars(5).build();
    label.add_css_class("numeric");
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    label
}

impl PlayerBar {
    pub fn new(window: &MainWindow) -> PlayerBar {
        // ── слева: трек ──
        let cover = Cover::new(48);
        let cover_button = gtk::Button::builder().child(&cover.root).valign(gtk::Align::Center).tooltip_text(tr("NowPlaying")).build();
        cover_button.add_css_class("flat");
        cover_button.add_css_class("cover-button");
        cover_button.set_action_name(Some("win.now-playing"));
        cover_button.update_property(&[gtk::accessible::Property::Label(tr(
            "PlayerArtwork.[using:Microsoft.UI.Xaml.Automation]AutomationProperties.Name",
        ))]);
        let title = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        title.add_css_class("heading");
        let subtitle = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        subtitle.add_css_class("dim-label");
        subtitle.add_css_class("caption");
        let error_label = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        error_label.add_css_class("error");
        error_label.add_css_class("caption");
        let retry = gtk::Button::builder().label(tr("Retry")).valign(gtk::Align::Center).build();
        retry.add_css_class("flat");
        retry.add_css_class("caption");
        let player = window.ctx.services.player.clone();
        retry.connect_clicked(move |_| player.send(Command::Retry));
        let error_box = gtk::Box::builder().spacing(6).visible(false).build();
        error_box.append(&error_label);
        error_box.append(&retry);
        let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).spacing(2).build();
        texts.append(&title);
        texts.append(&subtitle);
        texts.append(&error_box);
        let track = gtk::Box::builder().spacing(10).hexpand(false).build();
        track.append(&cover_button);
        track.append(&texts);

        // ── середина: кнопки и перемотка ──
        let shuffle = toggle_button(
            "media-playlist-shuffle-symbolic",
            tr("PlayerShuffle.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip"),
            tr("Shuffle"),
        );
        let previous = icon_button(
            "media-skip-backward-symbolic",
            tr("PlayerPrevious.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip"),
            tr("Previous"),
        );
        let play_icon = gtk::Image::from_icon_name("media-playback-start-symbolic");
        let spinner = adw::Spinner::new();
        let play_stack = gtk::Stack::new();
        play_stack.add_named(&play_icon, Some("icon"));
        play_stack.add_named(&spinner, Some("spinner"));
        let play =
            gtk::Button::builder().child(&play_stack).tooltip_text(format!("{} (Space)", tr("Play"))).valign(gtk::Align::Center).build();
        play.add_css_class("circular");
        play.add_css_class("play-button");
        let next = icon_button(
            "media-skip-forward-symbolic",
            tr("PlayerNext.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip"),
            tr("Next"),
        );
        let repeat = icon_button("media-playlist-consecutive-symbolic", &format!("{} (Ctrl+T)", tr("RepeatOff")), tr("RepeatOff"));
        let controls = gtk::Box::builder().spacing(6).halign(gtk::Align::Center).build();
        for widget in
            [shuffle.upcast_ref::<gtk::Widget>(), previous.upcast_ref(), play.upcast_ref(), next.upcast_ref(), repeat.upcast_ref()]
        {
            controls.append(widget);
        }

        let adjustment = gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 5.0, 0.0);
        let make_seek = |adjustment: &gtk::Adjustment| {
            let scale = gtk::Scale::builder().adjustment(adjustment).hexpand(true).draw_value(false).build();
            scale.update_property(&[gtk::accessible::Property::Label(tr(
                "PlayerSeek.[using:Microsoft.UI.Xaml.Automation]AutomationProperties.Name",
            ))]);
            scale
        };
        let (position_center, duration_center) = (time_label(), time_label());
        let seek_center = gtk::Box::builder().spacing(6).width_request(360).build();
        let center_scale = make_seek(&adjustment);
        seek_center.append(&position_center);
        seek_center.append(&center_scale);
        seek_center.append(&duration_center);
        let middle = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).build();
        middle.append(&controls);
        middle.append(&seek_center);

        // ── справа: очередь, громкость, «…» ──
        let queue = toggle_button(
            "view-list-bullet-symbolic",
            &format!("{} (Ctrl+U)", tr("QueueTitle")),
            tr("PlayerQueueButton.[using:Microsoft.UI.Xaml.Automation]AutomationProperties.Name"),
        );
        queue.set_action_name(Some("win.queue"));
        let volume = gtk::Adjustment::new(window.ctx.settings.get(&keys::VOLUME).clamp(0.0, 1.0), 0.0, 1.0, 0.02, 0.1, 0.0);
        let mute_tooltip = tr("PlayerMute.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip");
        let mute_inline = toggle_button(
            "audio-volume-high-symbolic",
            mute_tooltip,
            tr("PlayerMute.[using:Microsoft.UI.Xaml.Automation]AutomationProperties.Name"),
        );
        let inline_scale =
            gtk::Scale::builder().adjustment(&volume).width_request(110).draw_value(false).valign(gtk::Align::Center).build();
        inline_scale.update_property(&[gtk::accessible::Property::Label(tr(
            "PlayerVolume.[using:Microsoft.UI.Xaml.Automation]AutomationProperties.Name",
        ))]);
        let volume_inline = gtk::Box::builder().spacing(2).build();
        volume_inline.append(&mute_inline);
        volume_inline.append(&inline_scale);
        // Узкое окно: громкость кнопкой с поповером — «Без звука», ползунок и число (§5.2).
        let mute_popover = toggle_button(
            "audio-volume-high-symbolic",
            mute_tooltip,
            tr("PlayerMute.[using:Microsoft.UI.Xaml.Automation]AutomationProperties.Name"),
        );
        let popover_scale = gtk::Scale::builder().adjustment(&volume).width_request(180).draw_value(false).build();
        let volume_value = gtk::Label::builder().width_chars(4).build();
        volume_value.add_css_class("numeric");
        let popover_box = gtk::Box::builder().spacing(6).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
        popover_box.append(&mute_popover);
        popover_box.append(&popover_scale);
        popover_box.append(&volume_value);
        let volume_button = gtk::MenuButton::builder()
            .icon_name("audio-volume-high-symbolic")
            .tooltip_text(tr("PlayerVolumeButton.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip"))
            .popover(&gtk::Popover::builder().child(&popover_box).build())
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        volume_button.add_css_class("flat");
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text(tr("PlayerMore.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip"))
            .valign(gtk::Align::Center)
            .build();
        more.add_css_class("flat");
        let actions = gtk::Box::builder().spacing(2).halign(gtk::Align::End).build();
        actions.append(&queue);
        actions.append(&volume_inline);
        actions.append(&volume_button);
        actions.append(&more);

        let main = gtk::CenterBox::builder().margin_start(8).margin_end(8).margin_top(6).margin_bottom(6).build();
        main.set_start_widget(Some(&track));
        main.set_center_widget(Some(&middle));
        main.set_end_widget(Some(&actions));
        let (position_top, duration_top) = (time_label(), time_label());
        let seek_top = gtk::Box::builder().spacing(6).margin_start(12).margin_end(12).margin_top(4).visible(false).build();
        seek_top.append(&position_top);
        seek_top.append(&make_seek(&adjustment));
        seek_top.append(&duration_top);
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).visible(false).build();
        root.add_css_class("player-bar");
        root.append(&seek_top);
        root.append(&main);

        let bar = PlayerBar(Rc::new(Inner {
            root,
            cover,
            title,
            subtitle,
            error_box,
            error_label,
            shuffle,
            previous,
            play,
            play_stack,
            next,
            repeat,
            adjustment,
            seek_center,
            seek_top,
            position_labels: [position_center, position_top],
            duration_labels: [duration_center, duration_top],
            queue,
            volume_inline,
            volume_button,
            volume,
            mute_buttons: [mute_inline, mute_popover],
            volume_value,
            more,
            state: RefCell::default(),
            seeking: Cell::new(false),
            seek_token: Cell::new(0),
            resolving_since: Cell::new(None),
            updating: Cell::new(false),
        }));
        bar.connect(window);
        bar.set_hidden(Hidden::default());
        bar
    }

    fn connect(&self, window: &MainWindow) {
        let player = window.ctx.services.player.clone();
        self.play.connect_clicked({
            let player = player.clone();
            move |_| player.send(Command::TogglePlay)
        });
        self.previous.set_action_name(Some("win.previous"));
        self.next.set_action_name(Some("win.next"));
        self.shuffle.set_action_name(Some("win.shuffle"));
        self.repeat.set_action_name(Some("win.repeat"));

        // Перемотка: позиция летит в плеер через 150 мс после последнего движения ползунка.
        let weak = Rc::downgrade(&self.0);
        self.adjustment.connect_value_changed(move |adjustment| {
            let Some(bar) = weak.upgrade() else { return };
            if bar.updating.get() {
                return;
            }
            bar.seeking.set(true);
            let text = format_duration((adjustment.value() * 1000.0) as i64);
            for label in &bar.position_labels {
                label.set_label(&text);
            }
            let token = bar.seek_token.get() + 1;
            bar.seek_token.set(token);
            let (weak, player) = (Rc::downgrade(&bar), player.clone());
            glib::timeout_add_local_once(Duration::from_millis(150), move || {
                if let Some(bar) = weak.upgrade() {
                    if bar.seek_token.get() == token {
                        player.send(Command::Seek(Duration::from_secs_f64(bar.adjustment.value())));
                        bar.seeking.set(false);
                    }
                }
            });
        });

        // Громкость: настройка и плеер; колесо мыши над кнопкой тоже меняет её (§5.2).
        let (settings, player) = (window.ctx.settings.clone(), window.ctx.services.player.clone());
        let weak = Rc::downgrade(&self.0);
        self.volume.connect_value_changed(move |adjustment| {
            settings.set(&keys::VOLUME, adjustment.value());
            player.send(Command::Settings(crate::services::playback_settings(&settings)));
            if let Some(bar) = weak.upgrade() {
                PlayerBar(bar).update_volume_icon(&settings);
            }
        });
        for mute in &self.mute_buttons {
            mute.set_action_name(Some("win.mute"));
        }
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let volume = self.volume.clone();
        scroll.connect_scroll(move |_, _, dy| {
            volume.set_value((volume.value() - dy * 0.05).clamp(0.0, 1.0));
            glib::Propagation::Stop
        });
        self.volume_button.add_controller(scroll);
        self.update_volume_icon(&window.ctx.settings);
    }

    pub fn update_volume_icon(&self, settings: &crate::settings_store::SettingsStore) {
        let (volume, muted) = (settings.get(&keys::VOLUME), settings.get(&keys::MUTED));
        let icon = if muted || volume <= 0.0 {
            "audio-volume-muted-symbolic"
        } else if volume < 0.34 {
            "audio-volume-low-symbolic"
        } else if volume < 0.67 {
            "audio-volume-medium-symbolic"
        } else {
            "audio-volume-high-symbolic"
        };
        for mute in &self.mute_buttons {
            mute.set_icon_name(icon);
            mute.set_active(muted);
        }
        self.volume_button.set_icon_name(icon);
        self.volume_value.set_label(&format!("{}", (volume * 100.0).round() as i64));
    }

    pub fn set_hidden(&self, hidden: Hidden) {
        self.more.set_menu_model(Some(&player_menu(hidden)));
    }

    pub fn set_volume(&self, value: f64) {
        self.volume.set_value(value.clamp(0.0, 1.0));
    }

    pub fn volume(&self) -> f64 {
        self.volume.value()
    }

    pub fn apply(&self, window: &MainWindow, state: &State) {
        let previous = self.state.replace(state.clone());
        self.root.set_visible(state.track.is_some());
        if let Some(track) = &state.track {
            if previous.track.as_ref().map(|t| &t.video_id) != Some(&track.video_id) {
                let fallback = melogold_core::thumbnails::for_video(&track.video_id, 120);
                self.cover.set(&window.ctx.services.images, track.thumbnail_url.as_deref().or(Some(&fallback)), 120);
            }
            self.title.set_label(&track.title);
            self.title.set_tooltip_text(Some(&track.title));
            self.subtitle.set_label(&track.subtitle());
        }
        let resolving = matches!(state.status, Status::Resolving | Status::Buffering) && state.playing;
        match (resolving, self.resolving_since.get()) {
            (true, None) => self.resolving_since.set(Some(Instant::now())),
            (false, Some(_)) => self.resolving_since.set(None),
            _ => {}
        }
        self.play_stack.set_visible_child_name(if resolving { "spinner" } else { "icon" });
        let icon = self.play_stack.child_by_name("icon").and_downcast::<gtk::Image>();
        if let Some(icon) = icon {
            icon.set_icon_name(Some(if state.playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" }));
        }
        let play_name = tr(if state.playing { "Pause" } else { "Play" });
        self.play.set_tooltip_text(Some(&format!("{play_name} (Space)")));
        self.play.update_property(&[gtk::accessible::Property::Label(play_name)]);
        match &state.error {
            Some(error) if state.status == Status::Error => {
                self.error_label.set_label(&texts::error_title(error));
                self.error_box.set_tooltip_text(Some(&texts::error_text(error)));
                self.error_box.set_visible(true);
                self.subtitle.set_visible(false);
            }
            _ => {
                self.error_box.set_visible(false);
                self.subtitle.set_visible(true);
            }
        }
        self.next.set_sensitive(state.has_next);
        self.previous.set_sensitive(state.has_previous);
        let (repeat_icon, repeat_key) = match state.repeat {
            RepeatMode::Off => ("media-playlist-consecutive-symbolic", "RepeatOff"),
            RepeatMode::All => ("media-playlist-repeat-symbolic", "RepeatAll"),
            RepeatMode::One => ("media-playlist-repeat-song-symbolic", "RepeatOne"),
        };
        self.repeat.set_icon_name(repeat_icon);
        self.repeat.set_tooltip_text(Some(&format!("{} (Ctrl+T)", tr(repeat_key))));
        self.repeat.update_property(&[gtk::accessible::Property::Label(tr(repeat_key))]);
        if state.repeat == RepeatMode::Off {
            self.repeat.remove_css_class("accent");
        } else {
            self.repeat.add_css_class("accent");
        }
        let duration = state.duration.unwrap_or_default();
        self.updating.set(true);
        self.adjustment.set_upper(duration.as_secs_f64().max(1.0));
        self.updating.set(false);
        let text = if state.track.as_ref().is_some_and(|t| t.is_live()) {
            "LIVE".to_owned()
        } else {
            format_duration(duration.as_millis() as i64)
        };
        for label in &self.duration_labels {
            label.set_label(&text);
        }
    }

    /// Позиция из плеера — несколько раз в секунду; через 3 с ожидания — «Получаем поток…».
    pub fn tick(&self, position: Option<Duration>) {
        if let Some(since) = self.resolving_since.get() {
            if since.elapsed() > Duration::from_secs(3) {
                self.subtitle.set_label(tr("ResolvingText.Text"));
            }
        } else if let Some(track) = &self.state.borrow().track {
            if self.subtitle.label() != track.subtitle() {
                self.subtitle.set_label(&track.subtitle());
            }
        }
        if self.seeking.get() {
            return;
        }
        let position = position.unwrap_or_default();
        self.updating.set(true);
        self.adjustment.set_value(position.as_secs_f64().min(self.adjustment.upper()));
        self.updating.set(false);
        let text = format_duration(position.as_millis() as i64);
        for label in &self.position_labels {
            label.set_label(&text);
        }
    }
}

/// Какие кнопки панели сейчас спрятаны порогом ширины: они — в начале «…» (§5.2), а когда
/// видны, в меню их нет — двух одинаковых действий рядом не бывает (решение пользователя №6).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Hidden {
    pub modes: bool,
    pub queue: bool,
}

/// Меню «…» панели плеера — одно на всё (§5.3). Трек, текст и таймер добавят свои срезы.
pub fn player_menu(hidden: Hidden) -> gio::Menu {
    let menu = gio::Menu::new();
    let panel = gio::Menu::new();
    if hidden.queue {
        panel.append(Some(tr("QueueTitle")), Some("win.queue"));
    }
    if hidden.modes {
        panel.append(Some(tr("Shuffle")), Some("win.shuffle"));
        panel.append(Some(tr("RepeatAll")), Some("win.repeat"));
    }
    if panel.n_items() > 0 {
        menu.append_section(None, &panel);
    }
    let track = gio::Menu::new();
    track.append(Some(tr("MenuTrackRadio")), Some("win.current-radio"));
    track.append(Some(tr("MenuCopyLink")), Some("win.current-copy-link"));
    menu.append_section(None, &track);
    let info = gio::Menu::new();
    info.append(Some(tr("MenuStreamInfo")), Some("win.stream-info"));
    info.append(Some(tr("MenuShortcuts")), Some("app.shortcuts"));
    menu.append_section(None, &info);
    menu
}

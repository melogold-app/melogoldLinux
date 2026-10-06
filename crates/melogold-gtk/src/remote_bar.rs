//! Пульт другого устройства над панелью плеера (задание 0011): обложка, название, перемотка, громкость и
//! «Слушать здесь» / «Отключиться». Пока устройство выбрано, своя панель скрыта, а команды плееру (кнопки,
//! клавиши, нажатие по треку, MPRIS) уходят на выбранное устройство (`remote::redirect`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use melogold_core::text::format_duration;
use melogold_playback::engine::Command;
use melogold_server::remote::RemoteView;

use crate::localization::{tr, trf};
use crate::pages::account::device_icon;
use crate::widgets::Cover;
use crate::window::MainWindow;

/// Через сколько после последнего движения ползунка уходит команда (перемотка, громкость).
const DEBOUNCE: Duration = Duration::from_millis(150);

struct Bar {
    root: gtk::Box,
    cover: Cover,
    title: gtk::Label,
    subtitle: gtk::Label,
    play_icon: gtk::Image,
    seek: gtk::Adjustment,
    volume: gtk::Adjustment,
    position: gtk::Label,
    duration: gtk::Label,
    banner: gtk::Label,
    banner_icon: gtk::Image,
    view: RefCell<RemoteView>,
    /// Ползунок двигают руками: данные с устройства его не перебивают.
    seeking: Cell<bool>,
    volume_dragging: Cell<bool>,
    updating: Cell<bool>,
    seek_token: Cell<u64>,
    volume_token: Cell<u64>,
    shown_cover: RefCell<Option<String>>,
}

/// Низ окна: пульт (скрыт, пока устройство не выбрано) и над ним/под ним своя панель `local`.
pub fn bottom(window: &MainWindow, local: &gtk::Box) -> gtk::Box {
    let bar = Rc::new(build(window));
    connect_sliders(window, &bar);
    let local_wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
    local_wrap.append(local);
    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    holder.append(&bar.root);
    holder.append(&local_wrap);

    // Пульт следует за состоянием: канал хаба читается в главном потоке.
    let (views, weak_window) = (window.ctx.services.remote.views.clone(), window.downgrade());
    let (weak_bar, wrap) = (Rc::downgrade(&bar), local_wrap.downgrade());
    glib::spawn_future_local(async move {
        while let Ok(view) = views.recv().await {
            let (Some(window), Some(bar), Some(wrap)) = (weak_window.upgrade(), weak_bar.upgrade(), wrap.upgrade()) else { break };
            bar.apply(&window, &view);
            wrap.set_visible(view.target.is_none());
        }
    });
    // Позиция идёт сама от `at`, пока играет: ползунок и время 4 раза в секунду.
    let (weak_window, weak_bar) = (window.downgrade(), Rc::downgrade(&bar));
    glib::timeout_add_local(Duration::from_millis(250), move || {
        let (Some(window), Some(bar)) = (weak_window.upgrade(), weak_bar.upgrade()) else { return glib::ControlFlow::Break };
        bar.tick(&window);
        glib::ControlFlow::Continue
    });
    // Пульт живёт, пока жива панель окна.
    let keep = RefCell::new(Some(bar));
    holder.connect_destroy(move |_| {
        keep.take();
    });
    holder
}

fn label(class: &str) -> gtk::Label {
    let label = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    label.add_css_class(class);
    label
}

fn icon_button(icon: &str, name: &str) -> gtk::Button {
    let button = gtk::Button::builder().icon_name(icon).tooltip_text(name).valign(gtk::Align::Center).build();
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

fn build(window: &MainWindow) -> Bar {
    let control = window.ctx.services.remote.control.clone();

    // Верх: обложка, название и кнопки.
    let cover = Cover::new(48);
    let title = label("heading");
    let subtitle = label("dim-label");
    subtitle.add_css_class("caption");
    let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).hexpand(true).spacing(2).build();
    texts.append(&title);
    texts.append(&subtitle);
    let previous = icon_button("media-skip-backward-symbolic", tr("Previous"));
    let play_icon = gtk::Image::from_icon_name("media-playback-start-symbolic");
    let play = gtk::Button::builder().child(&play_icon).tooltip_text(tr("Play")).valign(gtk::Align::Center).build();
    play.add_css_class("circular");
    play.add_css_class("play-button");
    let next = icon_button("media-skip-forward-symbolic", tr("Next"));
    let controls = gtk::Box::builder().spacing(6).build();
    controls.append(&previous);
    controls.append(&play);
    controls.append(&next);
    let top = gtk::Box::builder().spacing(10).margin_start(8).margin_end(8).margin_top(6).build();
    top.append(&cover.root);
    top.append(&texts);
    top.append(&controls);

    // Перемотка.
    let seek = gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 5.0, 0.0);
    let scale = gtk::Scale::builder().adjustment(&seek).hexpand(true).draw_value(false).build();
    scale.update_property(&[gtk::accessible::Property::Label(tr(
        "PlayerSeek.[using:Microsoft.UI.Xaml.Automation]AutomationProperties.Name",
    ))]);
    let (position, duration) = (time_label(), time_label());
    let progress = gtk::Box::builder().spacing(6).margin_start(12).margin_end(12).build();
    progress.append(&position);
    progress.append(&scale);
    progress.append(&duration);

    // Низ: «Играет на …», громкость, кнопки.
    let banner_icon = gtk::Image::from_icon_name("computer-symbolic");
    let banner = label("heading");
    banner.set_hexpand(true);
    let volume = gtk::Adjustment::new(0.0, 0.0, 100.0, 1.0, 5.0, 0.0);
    let volume_scale = gtk::Scale::builder().adjustment(&volume).width_request(140).draw_value(false).valign(gtk::Align::Center).build();
    volume_scale.update_property(&[gtk::accessible::Property::Label(tr("LinuxRemoteVolume"))]);
    let volume_icon = gtk::Image::from_icon_name("audio-volume-high-symbolic");
    let volume_box = gtk::Box::builder().spacing(6).build();
    volume_box.append(&volume_icon);
    volume_box.append(&volume_scale);
    let listen = gtk::Button::builder().label(tr("LinuxRemoteListenHere")).valign(gtk::Align::Center).build();
    let disconnect = gtk::Button::builder().label(tr("LinuxRemoteDisconnect")).valign(gtk::Align::Center).build();
    listen.add_css_class("flat");
    disconnect.add_css_class("flat");
    let device = gtk::Box::builder().spacing(6).hexpand(true).build();
    device.append(&banner_icon);
    device.append(&banner);
    let bottom = adw::WrapBox::builder().child_spacing(12).line_spacing(0).margin_start(12).margin_end(8).margin_bottom(6).build();
    bottom.append(&device);
    bottom.append(&volume_box);
    let buttons = gtk::Box::builder().spacing(4).build();
    buttons.append(&listen);
    buttons.append(&disconnect);
    bottom.append(&buttons);

    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).visible(false).build();
    root.add_css_class("player-bar");
    root.add_css_class("remote-bar");
    root.append(&top);
    root.append(&progress);
    root.append(&bottom);

    let bar = Bar {
        root,
        cover,
        title,
        subtitle,
        play_icon,
        seek,
        volume,
        position,
        duration,
        banner,
        banner_icon,
        view: RefCell::default(),
        seeking: Cell::new(false),
        volume_dragging: Cell::new(false),
        updating: Cell::new(false),
        seek_token: Cell::new(0),
        volume_token: Cell::new(0),
        shown_cover: RefCell::default(),
    };

    // Кнопки шлют команды устройству.
    for (button, run) in [
        (
            &play,
            Box::new({
                let control = control.clone();
                move || control.toggle()
            }) as Box<dyn Fn()>,
        ),
        (
            &previous,
            Box::new({
                let control = control.clone();
                move || control.previous()
            }),
        ),
        (
            &next,
            Box::new({
                let control = control.clone();
                move || control.next()
            }),
        ),
        (
            &disconnect,
            Box::new({
                let control = control.clone();
                move || control.disconnect()
            }),
        ),
    ] {
        button.connect_clicked(move |_| run());
    }
    let weak = window.downgrade();
    listen.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            listen_here(&window);
        }
    });
    bar
}

impl Bar {
    fn apply(self: &Rc<Self>, window: &MainWindow, view: &RemoteView) {
        self.view.replace(view.clone());
        let Some(target) = &view.target else {
            self.root.set_visible(false);
            return;
        };
        self.root.set_visible(true);
        let (icon, kind) = device_icon(Some(&target.platform));
        self.banner_icon.set_icon_name(Some(icon));
        self.banner_icon.update_property(&[gtk::accessible::Property::Label(tr(kind))]);
        self.banner.set_label(&trf("LinuxRemotePlayingOnFormat", &[&target.name]));
        self.banner.set_tooltip_text(Some(&target.name));
        match view.now.as_ref() {
            Some(now) => {
                let track = now.track.as_ref().map(|t| window.display(t));
                self.title.set_label(track.as_ref().map(|t| t.title.as_str()).unwrap_or(tr("LinuxRemoteNothingPlaying")));
                self.subtitle.set_label(&track.as_ref().map(|t| t.subtitle()).unwrap_or_default());
                let url = track
                    .as_ref()
                    .and_then(|t| t.thumbnail_url.clone().or_else(|| Some(melogold_core::thumbnails::for_video(&t.video_id, 120))));
                if *self.shown_cover.borrow() != url {
                    self.cover.set(&window.ctx.services.images, url.as_deref(), 120);
                    self.shown_cover.replace(url);
                }
                self.play_icon.set_icon_name(Some(if now.playing {
                    "media-playback-pause-symbolic"
                } else {
                    "media-playback-start-symbolic"
                }));
                self.updating.set(true);
                self.seek.set_upper(now.duration_ms.filter(|d| *d > 0).map(|d| d as f64 / 1000.0).unwrap_or(1.0).max(1.0));
                let text = now.duration_ms.filter(|d| *d > 0).map(format_duration).unwrap_or_else(|| "0:00".into());
                self.duration.set_label(&text);
                if !self.volume_dragging.get() {
                    self.volume.set_value(now.volume.unwrap_or(0) as f64);
                }
                self.updating.set(false);
            }
            None => {
                self.title.set_label(tr("LinuxRemoteNothingPlaying"));
                self.subtitle.set_label("");
                self.play_icon.set_icon_name(Some("media-playback-start-symbolic"));
            }
        }
        self.tick(window);
    }

    /// Позиция от `at` управляемого устройства и времени сервера.
    fn tick(&self, window: &MainWindow) {
        if self.seeking.get() || !self.root.is_visible() {
            return;
        }
        let Some(now) = self.view.borrow().now.clone() else { return };
        let position = now.position_at(window.ctx.services.remote.clock.now());
        self.updating.set(true);
        self.seek.set_value(position as f64 / 1000.0);
        self.position.set_label(&format_duration(position));
        self.updating.set(false);
    }
}

/// Ползунки: перемотка и громкость идут командой через 150 мс после последнего движения.
fn connect_sliders(window: &MainWindow, bar: &Rc<Bar>) {
    let control = window.ctx.services.remote.control.clone();
    let (weak, control_seek) = (Rc::downgrade(bar), control.clone());
    bar.seek.connect_value_changed(move |adjustment| {
        let Some(bar) = weak.upgrade() else { return };
        if bar.updating.get() {
            return;
        }
        bar.seeking.set(true);
        bar.position.set_label(&format_duration((adjustment.value() * 1000.0) as i64));
        let token = bar.seek_token.get() + 1;
        bar.seek_token.set(token);
        let (weak, control) = (Rc::downgrade(&bar), control_seek.clone());
        glib::timeout_add_local_once(DEBOUNCE, move || {
            if let Some(bar) = weak.upgrade() {
                if bar.seek_token.get() == token {
                    control.seek_to((bar.seek.value() * 1000.0) as i64);
                    bar.seeking.set(false);
                }
            }
        });
    });
    let weak = Rc::downgrade(bar);
    bar.volume.connect_value_changed(move |adjustment| {
        let Some(bar) = weak.upgrade() else { return };
        if bar.updating.get() {
            return;
        }
        bar.volume_dragging.set(true);
        let token = bar.volume_token.get() + 1;
        bar.volume_token.set(token);
        let (weak, control, value) = (Rc::downgrade(&bar), control.clone(), adjustment.value());
        glib::timeout_add_local_once(DEBOUNCE, move || {
            if let Some(bar) = weak.upgrade() {
                if bar.volume_token.get() == token {
                    control.set_volume(value.round() as i64);
                    bar.volume_dragging.set(false);
                }
            }
        });
    });
}

/// «Слушать здесь»: очередь и место управляемого устройства — сюда, устройство ставится на паузу само
/// (`handoffFrom`, API §4.9), пульт выключается.
pub fn listen_here(window: &MainWindow) {
    let hub = std::sync::Arc::clone(&window.ctx.services.remote);
    let hub_task = std::sync::Arc::clone(&hub);
    let task = window.ctx.services.run(async move { hub_task.control.state_to_take().await });
    let weak = window.downgrade();
    glib::spawn_future_local(async move {
        let Some(window) = weak.upgrade() else { return };
        let Some(Some(state)) = task.await else {
            window.toast(tr("LinuxRemoteNothingToTake"));
            return;
        };
        let tracks: Vec<_> = state.queue.iter().map(|t| t.to_track()).collect();
        let index = usize::try_from(state.index).unwrap_or(0).min(tracks.len().saturating_sub(1));
        // Место — от `at`, как его видят другие устройства.
        let position = melogold_server::remote::extrapolate_position(
            state.position_ms,
            melogold_core::iso::parse(&state.at).unwrap_or_else(|| hub.clock.now()),
            state.playing,
            hub.clock.now(),
            melogold_server::remote::duration_of(state.duration_ms, tracks.get(index)),
        );
        hub.control.disconnect();
        hub.reporter.take_over(melogold_server::dto::PlaybackHandoffInput {
            device_id: state.device_id.clone(),
            session_id: state.session_id.clone(),
        });
        window.ctx.services.player.send_local(Command::PlayListAt {
            tracks,
            start: index,
            position: Duration::from_millis(position.max(0) as u64),
        });
        if !state.playing {
            window.ctx.services.player.send_local(Command::Pause);
        }
    });
}

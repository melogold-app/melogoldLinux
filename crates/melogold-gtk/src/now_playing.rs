//! «Сейчас играет» (docs/PROMPT.md §5.2): страница поверх окна, Esc и «Назад» закрывают — своей
//! «Свернуть» нет. Обложка песни — квадратом, кадр видео 16:9 — целиком прямоугольником. В широком
//! окне обложка слева, текст справа, по обе стороны от середины окна; в узком — переключатель
//! «Обложка · Текст» в заголовке.

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
use crate::lyrics_view::{LyricsPanel, SyncedView};
use crate::texts;
use crate::window::MainWindow;

/// От этой ширины страницы обложка и текст стоят рядом.
const WIDE: &str = "min-width: 860sp";

#[derive(Clone)]
pub struct NowPlaying(Rc<Inner>);

pub struct Inner {
    pub page: adw::NavigationPage,
    pub lyrics: Rc<LyricsPanel>,
    /// «Обложка · Текст» в узком окне.
    switcher: adw::ToggleGroup,
    header: adw::HeaderBar,
    slot: gtk::Box,
    right: gtk::Box,
    wide: Cell<bool>,
    lyrics_refresh: RefCell<Option<Rc<dyn Fn()>>>,
    frame: gtk::AspectFrame,
    picture: gtk::Picture,
    placeholder: gtk::Image,
    title: gtk::Label,
    subtitle: gtk::Label,
    heart: gtk::Button,
    error: gtk::Label,
    error_actions: adw::WrapBox,
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
        // Обложка уступает место тексту ошибки и кнопкам: окно 800×600 не переполняется.
        frame.set_size_request(160, 160);

        let title = gtk::Label::builder().wrap(true).xalign(0.0).build();
        title.add_css_class("title-2");
        let subtitle = gtk::Label::builder().wrap(true).xalign(0.0).build();
        subtitle.add_css_class("dim-label");
        // ♡ — справа от названия, как у Android (TitleBlock): отметить играющий трек в одно
        // нажатие, не открывая меню. В «…» этой страницы «В Избранное» поэтому нет.
        let heart = gtk::Button::builder()
            .icon_name("heart-outline-symbolic")
            .tooltip_text(tr("PlayerLike.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip"))
            .action_name("win.current-like")
            .valign(gtk::Align::Center)
            .build();
        heart.add_css_class("flat");
        heart.add_css_class("circular");
        heart.add_css_class("heart");
        heart.add_css_class("now-playing-heart");
        crate::library_view::set_heart(&heart, false);
        let title_link = crate::widgets::link_button(&title);
        let subtitle_link = crate::widgets::link_button(&subtitle);
        let weak_window = window.downgrade();
        title_link.connect_clicked(move |_| {
            if let Some(window) = weak_window.upgrade() {
                window.open_playing_album();
            }
        });
        let weak_window = window.downgrade();
        subtitle_link.connect_clicked(move |button| {
            if let Some(window) = weak_window.upgrade() {
                window.open_playing_artist(button.upcast_ref());
            }
        });
        let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).hexpand(true).build();
        texts.append(&title_link);
        texts.append(&subtitle_link);
        let heading = gtk::Box::builder().spacing(12).build();
        heading.append(&texts);
        heading.append(&heart);
        let error = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).visible(false).build();
        error.add_css_class("error");
        // Рядом с причиной — «Повторить · Пропустить · Другие версии» (задание 0001).
        // В узком окне кнопки переносятся на вторую строку, а не распирают окно.
        let error_actions = adw::WrapBox::builder()
            .child_spacing(6)
            .line_spacing(6)
            .justify(adw::JustifyMode::None)
            .align(0.5)
            .halign(gtk::Align::Center)
            .visible(false)
            .build();
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

        // Обложка или (узкое окно, «Текст») текст — сверху колонки, над названием и кнопками.
        let slot = gtk::Box::builder().orientation(gtk::Orientation::Vertical).vexpand(true).build();
        slot.append(&frame);
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        column.append(&slot);
        column.append(&heading);
        column.append(&error);
        column.append(&error_actions);
        column.append(&seek);
        column.append(&controls);
        let clamp = adw::Clamp::builder().maximum_size(560).child(&column).hexpand(true).build();
        let right = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).visible(false).margin_end(12).build();
        let halves = gtk::Box::builder().homogeneous(true).build();
        halves.append(&clamp);
        halves.append(&right);
        let bin = adw::BreakpointBin::builder().width_request(360).height_request(200).child(&halves).build();

        let (player, weak_window) = (window.ctx.services.player.clone(), window.downgrade());
        let seek_player = player.clone();
        let synced = SyncedView::new(
            Box::new(move || player.position()),
            Box::new(move || weak_window.upgrade().is_some_and(|w| w.is_playing())),
            Rc::new(move |ms| seek_player.send(Command::Seek(Duration::from_millis(ms.max(0) as u64)))),
        );
        let lyrics = LyricsPanel::new(synced);

        let switcher = adw::ToggleGroup::builder().valign(gtk::Align::Center).visible(false).build();
        switcher.add(adw::Toggle::builder().name("cover").label(tr("NowPlayingArtwork")).build());
        switcher.add(adw::Toggle::builder().name("lyrics").label(tr("PlayerLyrics")).build());
        switcher.set_active_name(Some("cover"));
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&switcher));
        // «…» — то же меню, что у панели плеера: панели на этой странице нет, а группа «Текст»
        // (редактор, другой текст, сдвиг) живёт только там и только пока текст на экране.
        // Перемешать, повтор и ♡ — кнопки страницы, в меню их нет.
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text(tr("PlayerMore.[using:Microsoft.UI.Xaml.Controls]ToolTipService.ToolTip"))
            .build();
        let weak_window = window.downgrade();
        more.set_create_popup_func(move |button| {
            if let Some(window) = weak_window.upgrade() {
                let hidden = crate::player_bar::Hidden { modes: false, queue: false, heart: false };
                button.set_menu_model(Some(&crate::player_bar::player_menu(&window, hidden)));
            }
        });
        header.pack_end(&more);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&bin));
        let page = adw::NavigationPage::builder().title(tr("NowPlaying")).tag("now-playing").child(&toolbar).build();

        let now_playing = NowPlaying(Rc::new(Inner {
            page,
            lyrics,
            switcher,
            header,
            slot,
            right,
            wide: Cell::new(false),
            lyrics_refresh: RefCell::default(),
            frame,
            picture,
            placeholder,
            title,
            subtitle,
            heart,
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

        // Широко — обложка и текст рядом; узко — одно из двух по переключателю.
        let wide = adw::Breakpoint::new(adw::BreakpointCondition::parse(WIDE).expect("условие порога"));
        let relayout = |wide: Option<bool>| {
            let (weak, weak_window) = (Rc::downgrade(&now_playing.0), window.downgrade());
            move || {
                if let (Some(inner), Some(window)) = (weak.upgrade(), weak_window.upgrade()) {
                    if let Some(wide) = wide {
                        inner.wide.set(wide);
                    }
                    NowPlaying(inner).place(&window);
                }
            }
        };
        let apply = relayout(Some(true));
        wide.connect_apply(move |_| apply());
        let unapply = relayout(Some(false));
        wide.connect_unapply(move |_| unapply());
        bin.add_breakpoint(wide);
        let switched = relayout(None);
        now_playing.switcher.connect_active_name_notify(move |_| switched());
        let shown = relayout(None);
        now_playing.page.connect_shown(move |_| shown());
        let hidden = relayout(None);
        now_playing.page.connect_hidden(move |_| hidden());
        // Текст перерисовывается сам, когда сервис текста меняет состояние.
        let (weak, service) = (Rc::downgrade(&now_playing.lyrics), window.lyrics.clone());
        let refresh: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(panel) = weak.upgrade() {
                panel.show(&service);
            }
        });
        window.lyrics.listen(&refresh);
        now_playing.lyrics_refresh.replace(Some(refresh));
        now_playing.place(window);
        now_playing
    }

    /// Текст на экране: страница открыта, и текст виден (широкое окно или «Текст» в узком).
    pub fn lyrics_visible(&self, window: &MainWindow) -> bool {
        window.now_playing_open() && (self.wide.get() || self.switcher.active_name().as_deref() == Some("lyrics"))
    }

    /// Показать текст (Ctrl+L, кнопка «Текст»): в узком окне — переключить на «Текст».
    pub fn show_lyrics(&self) {
        self.switcher.set_active_name(Some("lyrics"));
    }

    /// Разложить обложку и текст по ширине и переключателю; сервис текста ищет, только пока текст виден.
    fn place(&self, window: &MainWindow) {
        let wide = self.wide.get();
        let lyrics_narrow = !wide && self.switcher.active_name().as_deref() == Some("lyrics");
        // Широко переключателя нет — в заголовке название страницы.
        self.switcher.set_visible(!wide);
        self.header.set_title_widget(if wide { None } else { Some(&self.switcher) });
        self.right.set_visible(wide);
        self.frame.set_visible(!lyrics_narrow);
        let target = if wide {
            Some(&self.right)
        } else if lyrics_narrow {
            Some(&self.slot)
        } else {
            None
        };
        let root = &self.lyrics.root;
        let parent = root.parent().and_downcast::<gtk::Box>();
        if parent.as_ref() != target {
            if let Some(parent) = parent {
                parent.remove(root);
            }
            if let Some(target) = target {
                target.append(root);
            }
        }
        let open = window.now_playing_open();
        window.lyrics.set_active(open && (wide || lyrics_narrow));
        window.update_lyrics_button();
    }

    pub fn set_liked(&self, liked: bool) {
        crate::library_view::set_heart(&self.heart, liked);
    }

    pub fn apply(&self, window: &MainWindow, state: &State) {
        if let Some(track) = &state.track {
            self.set_liked(window.library_view.is_liked(&track.video_id));
            self.title.set_label(&track.title);
            self.subtitle.set_label(&track.subtitle());
            crate::widgets::update_track_links(&self.title, &self.subtitle, track);
            let source = track.thumbnail_url.clone().unwrap_or_else(|| thumbnails::for_video(&track.video_id, 544));
            let url = thumbnails::sized(Some(&source), 544);
            if *self.requested.borrow() != url {
                // Пока картинка грузится: кадр видео 16:9 — прямоугольником, обложка песни — квадратом (§5.2, §5.5).
                let wide = thumbnails::is_wide(track.thumbnail_url.as_deref()) || (track.thumbnail_url.is_none() && track.is_video());
                self.frame.set_ratio(if wide { 16.0 / 9.0 } else { 1.0 });
                self.requested.replace(url.clone());
                self.picture.set_paintable(gtk::gdk::Paintable::NONE);
                self.placeholder.set_visible(true);
                if let Some(url) = url {
                    let (this, images) = (self.clone(), window.ctx.services.images.clone());
                    glib::spawn_future_local(async move {
                        let texture = images.load(url.clone()).await;
                        if this.requested.borrow().as_deref() == Some(url.as_str()) {
                            if let Some(texture) = texture {
                                // Форма — по картинке без полей: шире 1,2:1 — 16:9, иначе квадрат
                                // (обложка сингла из видео-«статики»; задание Windows 0007).
                                let ratio = f64::from(texture.width()) / f64::from(texture.height().max(1));
                                this.frame.set_ratio(if ratio > 1.2 { 16.0 / 9.0 } else { 1.0 });
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

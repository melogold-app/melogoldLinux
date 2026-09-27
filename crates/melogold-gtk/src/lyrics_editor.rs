//! Редактор текста (`spec/lyrics.md` «Редактор», Windows `LyricsEditorView.cs`, Android
//! `LyricsEditorDialog`): вкладки «Текст», «Синхронизация», «Просмотр».
//!
//! - «Текст» — строка песни на строку, подпевка в скобках в конце. Набранное попадает в черновик при
//!   **любом** уходе с вкладки — смене вкладки, сохранении, экспорте (у Windows «Синхронизация»
//!   показывала старые строки: в обработчике смены вкладки выбранной уже была новая);
//! - «Синхронизация» — «Отметить» (Enter) во время воспроизведения ставит начало строки или слова,
//!   «Конец строки» (Shift+Enter) — паузу после неё. «Далее» — следующая строка **целиком**, с
//!   подпевкой, без многоточия; очень длинная прокручивается внутри блока (задание 0007);
//! - «Просмотр» — тот же синхронный текст, что в «Сейчас играет».
//!
//! Сохранённый текст — свой: TTML и обычный рядом; через 2 с он уходит на сервер.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use melogold_core::lyrics::draft::{DraftLine, LyricsDraft};
use melogold_core::lyrics::sync_rules::StoredLyrics;
use melogold_core::lyrics::{lrc, rows, ttml, LyricsTiming, SyncedLyrics, VocalSide};
use melogold_core::music::Track;
use melogold_playback::engine::Command;

use crate::localization::{tr, trf};
use crate::lyrics_view::SyncedView;
use crate::window::{MainWindow, WeakWindow};

const MAX_UNDO: usize = 200;
/// Отметка раньше нажатия: человек нажимает чуть позже, чем слышит.
const REACTION_MS: i64 = 150;
const REWIND_MS: i64 = 3_000;
const NUDGE_MS: i64 = 100;
/// Нажатие по отмеченной строке играет с чуть раньше неё.
const REPLAY_LEAD_MS: i64 = 2_000;
/// Выше этого блок «Далее» прокручивается внутри себя: «Отметить» не уезжает с экрана.
const NEXT_MAX_HEIGHT: i32 = 200;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Text,
    Sync,
    Preview,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Tab::Text => "text",
            Tab::Sync => "sync",
            Tab::Preview => "preview",
        }
    }

    fn from_name(name: &str) -> Tab {
        match name {
            "sync" => Tab::Sync,
            "preview" => Tab::Preview,
            _ => Tab::Text,
        }
    }
}

struct Editor {
    window: WeakWindow,
    track: Track,
    original: Option<StoredLyrics>,
    initial: LyricsDraft,
    draft: RefCell<LyricsDraft>,
    history: RefCell<Vec<LyricsDraft>>,
    /// Вкладка, которая сейчас на экране: в обработчике смены у переключателя уже новая.
    shown: Cell<Tab>,
    dialog: adw::Dialog,
    tabs: adw::ToggleGroup,
    stack: gtk::Stack,
    text: gtk::TextView,
    timing: adw::ToggleGroup,
    lines: gtk::ListBox,
    lines_scroller: gtk::ScrolledWindow,
    positions: [gtk::Label; 2],
    play_buttons: [gtk::Button; 2],
    next_caption: gtk::Label,
    next: gtk::Label,
    next_scroller: gtk::ScrolledWindow,
    next_shown: RefCell<String>,
    mark: gtk::Button,
    mark_end: gtk::Button,
    undo: gtk::Button,
    export_ttml: gio::SimpleAction,
    export_lrc: gio::SimpleAction,
    preview: Rc<SyncedView>,
    preview_stack: gtk::Stack,
    clock: RefCell<Option<glib::SourceId>>,
    /// Сам редактор — для строк, которые держат его слабо.
    this: RefCell<std::rc::Weak<Editor>>,
}

impl MainWindow {
    /// «Редактировать текст»: черновик — текст играющего трека во времени трека.
    pub fn edit_lyrics(&self) {
        let Some(track) = self.lyrics.track() else { return };
        let editor = Editor::new(self, track, self.lyrics.stored(), self.lyrics.initial_draft());
        editor.dialog.present(Some(&self.window));
    }
}

fn format_time(ms: i64) -> String {
    let separator = if crate::localization::lang() == crate::localization::Lang::Ru { ',' } else { '.' };
    format!("{}:{:02}{separator}{:02}", ms / 60_000, ms / 1000 % 60, ms / 10 % 100)
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).valign(gtk::Align::Center).build();
    button.add_css_class("flat");
    button
}

/// Строка в режиме слов: отмеченные слова — цветом акцента, следующее — жирным с чертой.
fn word_attrs(line: &DraftLine, word_cursor: Option<usize>) -> (String, pango::AttrList) {
    let words = line.words();
    let text = words.join(" ");
    let list = pango::AttrList::new();
    let dark = adw::StyleManager::default().is_dark();
    let accent = adw::StyleManager::default().accent_color().to_standalone_rgba(dark);
    let mut start = 0u32;
    for (index, word) in words.iter().enumerate() {
        let end = start + word.len() as u32;
        let marked = line.word_starts.get(index).copied().flatten().is_some();
        if marked {
            let mut color = pango::AttrColor::new_foreground(
                (accent.red() * 65535.0) as u16,
                (accent.green() * 65535.0) as u16,
                (accent.blue() * 65535.0) as u16,
            );
            color.set_start_index(start);
            color.set_end_index(end);
            list.insert(color);
        }
        if word_cursor == Some(index) {
            let mut bold = pango::AttrInt::new_weight(pango::Weight::Bold);
            bold.set_start_index(start);
            bold.set_end_index(end);
            list.insert(bold);
            let mut underline = pango::AttrInt::new_underline(pango::Underline::Single);
            underline.set_start_index(start);
            underline.set_end_index(end);
            list.insert(underline);
        }
        start = end + 1;
    }
    (text, list)
}

impl Editor {
    fn new(window: &MainWindow, track: Track, original: Option<StoredLyrics>, initial: LyricsDraft) -> Rc<Editor> {
        let shown = window.display(&track);
        let subtitle = match shown.artists_text.as_deref().filter(|a| !a.is_empty()) {
            Some(artist) => format!("{} · {artist}", shown.title),
            None => shown.title.clone(),
        };

        // ── заголовок: Отменить · «Текст песни» · ⋯ Сохранить ──
        let undo = icon_button("edit-undo-symbolic", &format!("{} (Ctrl+Z)", tr("LyricsEditorUndo")));
        undo.update_property(&[gtk::accessible::Property::Label(tr("LyricsEditorUndo"))]);
        let save = gtk::Button::builder().label(tr("LyricsEditorSave")).tooltip_text("Ctrl+S").build();
        save.add_css_class("suggested-action");
        let menu = gio::Menu::new();
        menu.append(Some(tr("LyricsEditorLanguage")), Some("editor.language"));
        let export = gio::Menu::new();
        export.append(Some(tr("LyricsEditorExportTtml")), Some("editor.export-ttml"));
        export.append(Some(tr("LyricsEditorExportLrc")), Some("editor.export-lrc"));
        menu.append_section(None, &export);
        let more = gtk::MenuButton::builder().icon_name("view-more-symbolic").menu_model(&menu).tooltip_text(tr("MoreOptions")).build();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new(tr("LyricsEditorTitle"), &subtitle)));
        header.pack_start(&undo);
        header.pack_end(&save);
        header.pack_end(&more);
        let tabs = adw::ToggleGroup::builder().halign(gtk::Align::Center).margin_bottom(6).build();
        tabs.add_css_class("editor-tabs");
        for (name, key) in [("text", "LyricsEditorTabText"), ("sync", "LyricsEditorTabSync"), ("preview", "LyricsEditorTabPreview")] {
            tabs.add(adw::Toggle::builder().name(name).label(tr(key)).build());
        }

        // ── «Текст» ──
        let text = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(12)
            .bottom_margin(12)
            .left_margin(12)
            .right_margin(12)
            .accepts_tab(false)
            .build();
        text.update_property(&[gtk::accessible::Property::Label(tr("LyricsEditorTextLabel"))]);
        text.buffer().set_text(&initial.to_text());
        let text_scroller = gtk::ScrolledWindow::builder().child(&text).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        text_scroller.add_css_class("card");
        text_scroller.set_overflow(gtk::Overflow::Hidden);
        let hint = gtk::Label::builder().label(tr("LyricsEditorTextHint")).wrap(true).xalign(0.0).build();
        hint.add_css_class("dim-label");
        hint.add_css_class("caption");
        let text_page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        text_page.append(&text_scroller);
        text_page.append(&hint);

        // ── «Синхронизация» ──
        let timing = adw::ToggleGroup::builder().halign(gtk::Align::Center).margin_bottom(4).build();
        timing.add_css_class("editor-timing");
        timing.add(adw::Toggle::builder().name("line").label(tr("LyricsEditorLines")).build());
        timing.add(adw::Toggle::builder().name("word").label(tr("LyricsEditorWords")).build());
        timing.set_active_name(Some(if initial.timing == LyricsTiming::Word { "word" } else { "line" }));
        let lines = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).margin_start(12).margin_end(12).build();
        lines.add_css_class("lyrics-editor-lines");
        let lines_scroller = gtk::ScrolledWindow::builder().child(&lines).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
        let transport = |position: &gtk::Label| {
            let row = gtk::Box::builder().spacing(8).build();
            let rewind = icon_button("media-seek-backward-symbolic", tr("LyricsEditorRewind"));
            rewind.set_action_name(Some("editor.rewind"));
            let play = icon_button("media-playback-start-symbolic", tr("Play"));
            play.set_action_name(Some("win.play-pause"));
            position.add_css_class("numeric");
            position.add_css_class("title-4");
            position.set_width_chars(8);
            position.set_xalign(0.0);
            row.append(&rewind);
            row.append(&play);
            row.append(position);
            (row, play)
        };
        let position = gtk::Label::new(Some("0:00,00"));
        let (sync_transport, sync_play) = transport(&position);
        let next_caption = gtk::Label::builder().label(tr("LinuxLyricsEditorNext")).xalign(0.0).build();
        next_caption.add_css_class("caption-heading");
        next_caption.add_css_class("dim-label");
        let next = gtk::Label::builder().wrap(true).wrap_mode(pango::WrapMode::WordChar).xalign(0.0).selectable(false).build();
        next.add_css_class("title-4");
        let next_scroller = gtk::ScrolledWindow::builder()
            .child(&next)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(NEXT_MAX_HEIGHT)
            .build();
        let next_block = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).build();
        next_block.append(&next_caption);
        next_block.append(&next_scroller);
        let mark_end = gtk::Button::builder().label(tr("LyricsEditorEndLine")).tooltip_text("Shift+Enter").height_request(48).build();
        mark_end.add_css_class("pill");
        let mark_label = gtk::Label::new(Some(tr("LyricsEditorMark")));
        mark_label.add_css_class("heading");
        let mark = gtk::Button::builder().child(&mark_label).tooltip_text("Enter").hexpand(true).height_request(48).build();
        mark.add_css_class("pill");
        mark.add_css_class("suggested-action");
        mark.update_property(&[gtk::accessible::Property::Label(tr("LyricsEditorMark"))]);
        let buttons = gtk::Box::builder().spacing(8).build();
        buttons.append(&mark_end);
        buttons.append(&mark);
        // Подсказка о клавишах — своей строкой под кнопками: в узком окне она переносится.
        let keys = gtk::Label::builder().label(tr("LyricsEditorKeysHint")).wrap(true).xalign(0.0).build();
        keys.add_css_class("dim-label");
        keys.add_css_class("caption");
        let controls = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(12)
            .build();
        controls.append(&sync_transport);
        controls.append(&next_block);
        controls.append(&buttons);
        controls.append(&keys);
        let controls_card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        controls_card.add_css_class("lyrics-editor-controls");
        controls_card.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        controls_card.append(&controls);
        let sync_page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        sync_page.append(&timing);
        sync_page.append(&lines_scroller);
        sync_page.append(&controls_card);

        // ── «Просмотр» ──
        let (player, weak_window) = (window.ctx.services.player.clone(), window.downgrade());
        let seek_player = player.clone();
        let preview = SyncedView::new(
            Box::new(move || player.position()),
            Box::new(move || weak_window.upgrade().is_some_and(|w| w.is_playing())),
            Rc::new(move |ms| seek_player.send(Command::Seek(Duration::from_millis(ms.max(0) as u64)))),
        );
        let no_lines = adw::StatusPage::builder().title(tr("LyricsEditorNoLines")).icon_name("lyrics-symbolic").build();
        no_lines.add_css_class("compact");
        let preview_stack = gtk::Stack::builder().vexpand(true).build();
        preview_stack.add_named(&preview.root, Some("lyrics"));
        preview_stack.add_named(&no_lines, Some("hint"));
        let preview_position = gtk::Label::new(Some("0:00,00"));
        let (preview_transport, preview_play) = transport(&preview_position);
        preview_transport.set_halign(gtk::Align::Center);
        preview_transport.set_margin_top(8);
        preview_transport.set_margin_bottom(12);
        let preview_page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        preview_page.append(&preview_stack);
        preview_page.append(&preview_transport);

        let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).vexpand(true).build();
        stack.add_named(&text_page, Some("text"));
        stack.add_named(&sync_page, Some("sync"));
        stack.add_named(&preview_page, Some("preview"));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&tabs);
        toolbar.set_content(Some(&stack));
        let dialog = adw::Dialog::builder().title(tr("LyricsEditorTitle")).content_width(760).content_height(720).child(&toolbar).build();
        dialog.set_can_close(false);

        let export_ttml = gio::SimpleAction::new("export-ttml", None);
        let export_lrc = gio::SimpleAction::new("export-lrc", None);
        let first = if initial.lines.is_empty() { Tab::Text } else { Tab::Sync };
        let editor = Rc::new(Editor {
            window: window.downgrade(),
            track,
            original,
            draft: RefCell::new(initial.clone()),
            initial,
            history: RefCell::default(),
            shown: Cell::new(first),
            dialog,
            tabs,
            stack,
            text,
            timing,
            lines,
            lines_scroller,
            positions: [position, preview_position],
            play_buttons: [sync_play, preview_play],
            next_caption,
            next,
            next_scroller,
            next_shown: RefCell::default(),
            mark,
            mark_end,
            undo,
            export_ttml,
            export_lrc,
            preview,
            preview_stack,
            clock: RefCell::default(),
            this: RefCell::default(),
        });
        editor.this.replace(Rc::downgrade(&editor));
        editor.connect(&save);
        editor.tabs.set_active_name(Some(first.name()));
        editor.show_tab();
        editor.show_undo();
        editor
    }

    fn connect(self: &Rc<Self>, save: &gtk::Button) {
        let actions = gio::SimpleActionGroup::new();
        let add = |name: &str, run: fn(&Rc<Editor>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(editor) = weak.upgrade() {
                    run(&editor);
                }
            });
            actions.add_action(&action);
        };
        add("language", |e| e.ask_language());
        add("rewind", |e| e.seek_by(-REWIND_MS));
        add("undo", |e| e.undo());
        add("save", |e| e.save());
        for (action, extension) in [(&self.export_ttml, "ttml"), (&self.export_lrc, "lrc")] {
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(editor) = weak.upgrade() {
                    editor.export(extension);
                }
            });
            actions.add_action(action);
        }
        self.dialog.insert_action_group("editor", Some(&actions));
        self.undo.set_action_name(Some("editor.undo"));
        save.set_action_name(Some("editor.save"));
        let shortcuts = gtk::ShortcutController::new();
        for (trigger, action) in [("<Control>z", "editor.undo"), ("<Control>s", "editor.save")] {
            shortcuts.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string(trigger), Some(gtk::NamedAction::new(action))));
        }
        self.dialog.add_controller(shortcuts);

        let weak = Rc::downgrade(self);
        self.tabs.connect_active_name_notify(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.show_tab();
            }
        });
        let weak = Rc::downgrade(self);
        self.timing.connect_active_name_notify(move |group| {
            let Some(editor) = weak.upgrade() else { return };
            let timing = if group.active_name().as_deref() == Some("word") { LyricsTiming::Word } else { LyricsTiming::Line };
            if timing != editor.draft.borrow().timing {
                editor.apply(|d| d.with_timing(timing).moved_to(d.cursor));
            }
        });
        let weak = Rc::downgrade(self);
        self.text.buffer().connect_changed(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.show_undo();
            }
        });
        let weak = Rc::downgrade(self);
        self.mark.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.mark();
            }
        });
        let weak = Rc::downgrade(self);
        self.mark_end.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.mark_end();
            }
        });
        // Нажатие по строке: следующая отметка — она; у отмеченной — играть с чуть раньше.
        let weak = Rc::downgrade(self);
        self.lines.connect_row_activated(move |_, row| {
            let Some(editor) = weak.upgrade() else { return };
            let Ok(index) = usize::try_from(row.index()) else { return };
            let start = editor.draft.borrow().lines.get(index).and_then(|l| l.start_ms);
            editor.apply(|d| d.moved_to(index));
            if let Some(start) = start {
                editor.seek_to(start - REPLAY_LEAD_MS);
            }
        });

        // Клавиши «Синхронизации» — раньше кнопки в фокусе; поле ввода получает клавиши само.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(editor) = weak.upgrade() else { return glib::Propagation::Proceed };
            editor.key(key, modifiers)
        });
        self.dialog.add_controller(keys);

        let weak = Rc::downgrade(self);
        self.dialog.connect_close_attempt(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.close_attempt();
            }
        });
        let weak = Rc::downgrade(self);
        self.dialog.connect_map(move |_| {
            let Some(editor) = weak.upgrade() else { return };
            let weak = Rc::downgrade(&editor);
            let id = glib::timeout_add_local(Duration::from_millis(50), move || {
                let Some(editor) = weak.upgrade() else { return glib::ControlFlow::Break };
                editor.show_transport();
                glib::ControlFlow::Continue
            });
            if let Some(old) = editor.clock.replace(Some(id)) {
                old.remove();
            }
        });
        // Редактор жив, пока диалог открыт: остальные держат его слабо. Закрыли — отпускают.
        let keep = RefCell::new(Some(Rc::clone(self)));
        self.dialog.connect_closed(move |_| {
            if let Some(editor) = keep.take() {
                if let Some(clock) = editor.clock.take() {
                    clock.remove();
                }
            }
        });
    }

    fn window(&self) -> Option<MainWindow> {
        self.window.upgrade()
    }

    fn position(&self) -> i64 {
        self.window().and_then(|w| w.ctx.services.player.position()).map(|p| p.as_millis() as i64).unwrap_or(0)
    }

    fn seek_to(&self, ms: i64) {
        if let Some(window) = self.window() {
            window.ctx.services.player.send(Command::Seek(Duration::from_millis(ms.max(0) as u64)));
        }
    }

    fn seek_by(&self, delta: i64) {
        self.seek_to(self.position() + delta);
    }

    // ── черновик ──

    fn typed_text(&self) -> String {
        let buffer = self.text.buffer();
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).replace("\r\n", "\n")
    }

    /// На «Тексте» набрано то, чего ещё нет в черновике.
    fn text_pending(&self) -> bool {
        self.shown.get() == Tab::Text && self.typed_text() != self.draft.borrow().to_text()
    }

    fn changed(&self) -> bool {
        *self.draft.borrow() != self.initial || self.text_pending()
    }

    fn push_history(&self, draft: LyricsDraft) {
        let mut history = self.history.borrow_mut();
        history.push(draft);
        if history.len() > MAX_UNDO {
            history.remove(0);
        }
    }

    /// Набранное на «Тексте» — в черновик (Отменить — не по буквам).
    fn commit_text(&self) {
        if !self.text_pending() {
            return;
        }
        let current = self.draft.borrow().clone();
        let next = current.with_text(&self.typed_text());
        self.push_history(current);
        self.draft.replace(next);
    }

    /// Правка черновика: прежний — в «Отменить».
    fn apply(&self, transform: impl FnOnce(&LyricsDraft) -> LyricsDraft) {
        self.commit_text();
        let current = self.draft.borrow().clone();
        let next = transform(&current);
        if next == current {
            return;
        }
        self.push_history(current);
        self.draft.replace(next);
        self.refresh();
    }

    fn undo(&self) {
        if self.text_pending() {
            self.text.buffer().set_text(&self.draft.borrow().to_text());
            self.show_undo();
            return;
        }
        let Some(previous) = self.history.borrow_mut().pop() else { return };
        self.draft.replace(previous);
        self.refresh();
    }

    fn show_undo(&self) {
        self.undo.set_sensitive(!self.history.borrow().is_empty() || self.text_pending());
    }

    fn refresh(&self) {
        let timing = if self.draft.borrow().timing == LyricsTiming::Word { "word" } else { "line" };
        if self.timing.active_name().as_deref() != Some(timing) {
            self.timing.set_active_name(Some(timing));
        }
        match self.shown.get() {
            Tab::Text => self.text.buffer().set_text(&self.draft.borrow().to_text()),
            Tab::Sync => self.show_lines(),
            Tab::Preview => self.show_preview(),
        }
        self.show_undo();
        self.show_transport();
    }

    /// Смена вкладки: сначала набранное — в черновик по вкладке, которая была на экране.
    fn show_tab(&self) {
        self.commit_text();
        let tab = Tab::from_name(self.tabs.active_name().as_deref().unwrap_or("text"));
        self.shown.set(tab);
        self.stack.set_visible_child_name(tab.name());
        match tab {
            Tab::Text => self.text.buffer().set_text(&self.draft.borrow().to_text()),
            Tab::Sync => self.show_lines(),
            Tab::Preview => self.show_preview(),
        }
        self.show_undo();
        self.show_transport();
    }

    fn show_preview(&self) {
        match self.draft.borrow().to_synced() {
            Some(lyrics) => {
                let rows = Rc::new(rows::build(&lyrics));
                self.preview.set_lyrics(&rows, 0, None);
                self.preview_stack.set_visible_child_name("lyrics");
            }
            None => self.preview_stack.set_visible_child_name("hint"),
        }
    }

    // ── строки ──

    /// Строки: время, текст (в режиме слов отмеченные слова цветом, следующее — жирным с чертой),
    /// подпевка целиком, сторона и меню.
    fn show_lines(&self) {
        let Some(this) = self.this.borrow().upgrade() else { return };
        while let Some(child) = self.lines.first_child() {
            self.lines.remove(&child);
        }
        let draft = self.draft.borrow().clone();
        if draft.lines.is_empty() {
            let hint = gtk::Label::builder().label(tr("LyricsEditorNoLines")).wrap(true).margin_top(24).margin_bottom(24).build();
            hint.add_css_class("dim-label");
            let row = gtk::ListBoxRow::builder().child(&hint).activatable(false).build();
            self.lines.append(&row);
            return;
        }
        for (index, line) in draft.lines.iter().enumerate() {
            self.lines.append(&this.row(index, line, &draft));
        }
        // Следующая строка — на виду: две строки над ней.
        let target = draft.cursor.saturating_sub(2);
        let weak = Rc::downgrade(&this);
        glib::idle_add_local_once(move || {
            let Some(editor) = weak.upgrade() else { return };
            let Some(row) = editor.lines.row_at_index(target as i32) else { return };
            if let Some(bounds) = row.compute_bounds(&editor.lines) {
                let adjustment = editor.lines_scroller.vadjustment();
                let y = f64::from(bounds.y()).min(adjustment.upper() - adjustment.page_size()).max(0.0);
                adjustment.set_value(y);
            }
        });
    }

    fn row(self: &Rc<Self>, index: usize, line: &DraftLine, draft: &LyricsDraft) -> gtk::ListBoxRow {
        let is_cursor = index == draft.cursor;
        let word_cursor = (is_cursor && draft.timing == LyricsTiming::Word).then_some(draft.word_cursor);
        let end = line.side == VocalSide::End;
        let time = gtk::Label::builder()
            .label(line.start_ms.map(format_time).unwrap_or_else(|| tr("LyricsEditorNoTime").to_owned()))
            .width_chars(8)
            .xalign(1.0)
            .valign(gtk::Align::Center)
            .build();
        time.add_css_class("numeric");
        if line.start_ms.is_none() {
            time.add_css_class("dim-label");
        }
        let main = gtk::Label::builder()
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .xalign(if end { 1.0 } else { 0.0 })
            .justify(if end { gtk::Justification::Right } else { gtk::Justification::Left })
            .build();
        if word_cursor.is_some() || !line.word_starts.is_empty() {
            let (text, attrs) = word_attrs(line, word_cursor);
            main.set_label(&text);
            main.set_attributes(Some(&attrs));
        } else {
            main.set_label(&line.text);
        }
        let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).valign(gtk::Align::Center).build();
        texts.append(&main);
        if let Some(backing) = &line.backing {
            // Подпевка — целиком (задание 0007).
            let label = gtk::Label::builder().label(backing).wrap(true).xalign(if end { 1.0 } else { 0.0 }).build();
            label.add_css_class("caption");
            label.add_css_class("dim-label");
            texts.append(&label);
        }
        let side_name = format!("{}: {}", tr("LyricsEditorSide"), tr(if end { "LyricsEditorSideEnd" } else { "LyricsEditorSideStart" }));
        let side = icon_button(if end { "format-justify-right-symbolic" } else { "format-justify-left-symbolic" }, &side_name);
        side.update_property(&[gtk::accessible::Property::Label(&side_name)]);
        let weak = Rc::downgrade(self);
        side.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.apply(|d| d.with_side(index, if end { VocalSide::Start } else { VocalSide::End }));
            }
        });

        // Меню строки: время ±0,1 с, подпевка, язык строки, сбросить время.
        let row_actions = gio::SimpleActionGroup::new();
        let timed = line.start_ms.is_some();
        let add = |name: &str, enabled: bool, run: Box<dyn Fn(&Rc<Editor>)>| {
            let action = gio::SimpleAction::new(name, None);
            action.set_enabled(enabled);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(editor) = weak.upgrade() {
                    run(&editor);
                }
            });
            row_actions.add_action(&action);
        };
        add("earlier", timed, Box::new(move |e| e.apply(|d| d.nudge(index, -NUDGE_MS))));
        add("later", timed, Box::new(move |e| e.apply(|d| d.nudge(index, NUDGE_MS))));
        let backing = line.backing.clone().unwrap_or_default();
        add(
            "backing",
            true,
            Box::new(move |e| {
                let weak = Rc::downgrade(e);
                let value = backing.trim_start_matches('(').trim_end_matches(')').to_owned();
                e.ask(tr("LyricsEditorBackingLabel"), tr("LyricsEditorBackingLabel"), &value, move |text| {
                    if let Some(editor) = weak.upgrade() {
                        editor.apply(|d| d.with_backing(index, Some(&text)));
                    }
                });
            }),
        );
        let language = line.language.clone().unwrap_or_default();
        add(
            "language",
            true,
            Box::new(move |e| {
                let weak = Rc::downgrade(e);
                e.ask(tr("LyricsEditorLanguageTitle"), tr("LyricsEditorLanguageLabel"), &language, move |code| {
                    if let Some(editor) = weak.upgrade() {
                        editor.apply(|d| d.with_line_language(index, Some(&code)));
                    }
                });
            }),
        );
        add("clear", timed, Box::new(move |e| e.apply(|d| d.clear_timing(index).moved_to(index))));
        let menu = gio::Menu::new();
        let time_section = gio::Menu::new();
        time_section.append(Some(tr("LyricsEditorEarlier")), Some("row.earlier"));
        time_section.append(Some(tr("LyricsEditorLater")), Some("row.later"));
        menu.append_section(None, &time_section);
        let line_section = gio::Menu::new();
        line_section.append(Some(tr("LyricsEditorBacking")), Some("row.backing"));
        line_section.append(Some(tr("LyricsEditorLineLanguage")), Some("row.language"));
        menu.append_section(None, &line_section);
        let clear_section = gio::Menu::new();
        clear_section.append(Some(tr("LyricsEditorClearTime")), Some("row.clear"));
        menu.append_section(None, &clear_section);
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&menu)
            .tooltip_text(tr("MoreOptions"))
            .valign(gtk::Align::Center)
            .build();
        more.add_css_class("flat");

        let content = gtk::Box::builder().spacing(12).margin_top(8).margin_bottom(8).margin_start(8).margin_end(4).build();
        content.append(&time);
        content.append(&texts);
        content.append(&side);
        content.append(&more);
        let row = gtk::ListBoxRow::builder().child(&content).build();
        row.insert_action_group("row", Some(&row_actions));
        row.add_css_class("lyrics-editor-line");
        if is_cursor {
            row.add_css_class("cursor");
        }
        let time_text = line.start_ms.map(format_time).unwrap_or_else(|| tr("LyricsEditorNoTime").to_owned());
        row.update_property(&[gtk::accessible::Property::Label(&format!("{time_text} {}", line.full_text()))]);
        row
    }

    // ── отметки ──

    fn mark(&self) {
        let draft = self.draft.borrow().clone();
        if draft.cursor >= draft.lines.len() {
            return;
        }
        let position = (self.position() - REACTION_MS).max(0);
        self.apply(|d| d.mark(position));
    }

    fn mark_end(&self) {
        if self.draft.borrow().cursor == 0 {
            return;
        }
        let position = (self.position() - REACTION_MS).max(0);
        self.apply(|d| d.mark_end(position));
    }

    /// Строка последней отметки: текущая, если в ней уже отмечены слова, иначе предыдущая.
    fn last_marked(&self) -> Option<usize> {
        let draft = self.draft.borrow();
        if draft.timing == LyricsTiming::Word && draft.word_cursor > 0 {
            Some(draft.cursor)
        } else {
            draft.cursor.checked_sub(1)
        }
    }

    /// Backspace: снять отметку последней строки, вернуться к ней и перемотать чуть раньше.
    fn remark(&self) {
        let Some(index) = self.last_marked() else { return };
        let Some(line) = self.draft.borrow().lines.get(index).cloned() else { return };
        self.apply(|d| d.clear_timing(index).moved_to(index));
        if let Some(start) = line.start_ms {
            self.seek_to(start - REPLAY_LEAD_MS);
        }
    }

    /// ↑ ↓: следующей отметить строку выше или ниже; у отмеченной — играть с чуть раньше.
    fn move_cursor(&self, delta: i64) {
        let (index, start) = {
            let draft = self.draft.borrow();
            let count = draft.lines.len();
            let index = (draft.cursor as i64 + delta).clamp(0, count.saturating_sub(1) as i64) as usize;
            (index, draft.lines.get(index).and_then(|l| l.start_ms))
        };
        self.apply(|d| d.moved_to(index));
        if let Some(start) = start {
            self.seek_to(start - REPLAY_LEAD_MS);
        }
    }

    fn nudge_last(&self, delta: i64) {
        let Some(index) = self.last_marked() else { return };
        if self.draft.borrow().lines.get(index).and_then(|l| l.start_ms).is_some() {
            self.apply(|d| d.nudge(index, delta));
        }
    }

    /// Клавиши «Синхронизации» (все — в «Сочетаниях клавиш», F1): Enter — «Отметить», Shift+Enter —
    /// «Конец строки», Backspace — заново, ↑ ↓ — какую строку отметить, ← → — ±3 с, [ ] — последняя
    /// отметка ±0,1 с, пробел — пауза. Enter на кнопке, на которую перешли Tab, нажимает её.
    fn key(&self, key: gdk::Key, modifiers: gdk::ModifierType) -> glib::Propagation {
        if self.shown.get() == Tab::Text || modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
            return glib::Propagation::Proceed;
        }
        let focus = self.dialog.focus();
        if focus.as_ref().is_some_and(|f| f.is::<gtk::Text>() || f.is::<gtk::TextView>()) {
            return glib::Propagation::Proceed;
        }
        if self.shown.get() != Tab::Sync {
            if key == gdk::Key::space {
                let _ = self.dialog.activate_action("win.play-pause", None);
                return glib::Propagation::Stop;
            }
            return glib::Propagation::Proceed;
        }
        let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
        let keyboard_focus = self.window().is_some_and(|w| w.window.gets_focus_visible())
            && focus.as_ref().is_some_and(|f| f.is::<gtk::Button>() || f.is::<gtk::MenuButton>() || f.is::<gtk::ToggleButton>());
        match key {
            gdk::Key::Return | gdk::Key::KP_Enter if !keyboard_focus => {
                if shift {
                    self.mark_end();
                } else {
                    self.mark();
                }
            }
            gdk::Key::BackSpace => self.remark(),
            gdk::Key::Up if !shift => self.move_cursor(-1),
            gdk::Key::Down if !shift => self.move_cursor(1),
            gdk::Key::Left if !shift => self.seek_by(-REWIND_MS),
            gdk::Key::Right if !shift => self.seek_by(REWIND_MS),
            gdk::Key::bracketleft => self.nudge_last(-NUDGE_MS),
            gdk::Key::bracketright => self.nudge_last(NUDGE_MS),
            gdk::Key::space => {
                let _ = self.dialog.activate_action("win.play-pause", None);
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    /// Позиция, play/pause и что отметит следующее нажатие — строка целиком (задание 0007).
    fn show_transport(&self) {
        let text = format_time(self.position());
        for label in &self.positions {
            if label.label() != text {
                label.set_label(&text);
            }
        }
        let playing = self.window().is_some_and(|w| w.is_playing());
        for button in &self.play_buttons {
            button.set_icon_name(if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" });
            button.set_tooltip_text(Some(tr(if playing { "Pause" } else { "Play" })));
        }
        let draft = self.draft.borrow();
        let line = draft.lines.get(draft.cursor);
        let key = match line {
            Some(line) if draft.timing == LyricsTiming::Word && !line.words().is_empty() => {
                format!("{}\u{1f}{:?}\u{1f}{}", line.full_text(), line.word_starts, draft.word_cursor)
            }
            Some(line) => line.full_text(),
            None => String::new(),
        };
        if *self.next_shown.borrow() != key {
            self.next_shown.replace(key);
            match line {
                None => {
                    self.next_caption.set_visible(false);
                    self.next.set_attributes(None);
                    self.next.set_label(tr("LyricsEditorAllMarked"));
                }
                Some(line) => {
                    self.next_caption.set_visible(true);
                    if draft.timing == LyricsTiming::Word && !line.words().is_empty() {
                        let word = draft.word_cursor.min(line.words().len() - 1);
                        let (mut text, attrs) = word_attrs(line, Some(word));
                        if let Some(backing) = &line.backing {
                            text.push('\n');
                            text.push_str(backing);
                        }
                        self.next.set_label(&text);
                        self.next.set_attributes(Some(&attrs));
                    } else {
                        self.next.set_attributes(None);
                        let text = match &line.backing {
                            Some(backing) => format!("{}\n{backing}", line.text),
                            None => line.text.clone(),
                        };
                        self.next.set_label(&text);
                    }
                    self.next.update_property(&[gtk::accessible::Property::Label(&trf("LyricsEditorNextFormat", &[&line.full_text()]))]);
                }
            }
            // Длинная строка прокручивается внутри блока — с начала при каждой новой.
            self.next_scroller.vadjustment().set_value(0.0);
        }
        self.mark.set_sensitive(line.is_some());
        self.mark_end.set_sensitive(draft.cursor > 0);
        let timed = draft.has_timing();
        self.export_ttml.set_enabled(timed);
        self.export_lrc.set_enabled(timed);
    }

    // ── сохранить, закрыть, экспорт ──

    fn save(&self) {
        self.commit_text();
        let Some(window) = self.window() else { return };
        window.lyrics.save_draft(&self.track.video_id, self.original.as_ref(), &self.draft.borrow());
        window.toast(tr("LyricsEditorSaved"));
        self.dialog.force_close();
    }

    /// Закрыть; с несохранёнными правками — сначала спросить.
    fn close_attempt(&self) {
        if !self.changed() {
            self.dialog.force_close();
            return;
        }
        let confirm = adw::AlertDialog::new(Some(tr("LyricsEditorDiscardTitle")), None);
        confirm.add_response("cancel", tr("Cancel"));
        confirm.add_response("discard", tr("LyricsEditorDiscard"));
        confirm.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        confirm.set_default_response(Some("cancel"));
        confirm.set_close_response("cancel");
        let dialog = self.dialog.downgrade();
        confirm.connect_response(Some("discard"), move |_, _| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.force_close();
            }
        });
        confirm.present(Some(&self.dialog));
    }

    fn export(&self, extension: &'static str) {
        self.commit_text();
        let Some(lyrics) = self.draft.borrow().to_synced() else { return };
        let text = if extension == "ttml" { ttml::write(&lyrics) } else { write_lrc(&lyrics) };
        let Some(window) = self.window() else { return };
        let dialog = gtk::FileDialog::builder()
            .title(tr(if extension == "ttml" { "LyricsEditorExportTtml" } else { "LyricsEditorExportLrc" }).trim_end_matches('…'))
            .modal(true)
            .initial_name(format!("{}.{extension}", crate::save_file::file_name(&window.display(&self.track))))
            .build();
        if let Some(documents) = glib::user_special_dir(glib::UserDirectory::Documents) {
            dialog.set_initial_folder(Some(&gio::File::for_path(documents)));
        }
        let weak = window.downgrade();
        let parent = self.dialog.root().and_downcast::<gtk::Window>();
        dialog.save(parent.as_ref(), gio::Cancellable::NONE, move |result| {
            let (Some(window), Ok(file)) = (weak.upgrade(), result) else { return };
            glib::spawn_future_local(async move {
                let written = file.replace_contents_future(text.into_bytes(), None, false, gio::FileCreateFlags::REPLACE_DESTINATION).await;
                match written {
                    Ok(_) => window.toast(tr("LyricsEditorExported")),
                    Err((_, error)) => {
                        tracing::warn!(%error, "текст не экспортировался");
                        window.toast(tr("BackupFailed"));
                    }
                }
            });
        });
    }

    fn ask_language(self: &Rc<Self>) {
        self.commit_text();
        let weak = Rc::downgrade(self);
        let current = self.draft.borrow().language.clone().unwrap_or_default();
        self.ask(tr("LyricsEditorLanguageTitle"), tr("LyricsEditorLanguageLabel"), &current, move |code| {
            if let Some(editor) = weak.upgrade() {
                editor.apply(|d| d.with_language(Some(&code)));
            }
        });
    }

    /// Поле в окне: язык или подпевка; пустое — снять.
    fn ask(&self, title: &str, label: &str, value: &str, done: impl Fn(String) + 'static) {
        let dialog = adw::AlertDialog::new(Some(title), None);
        let entry = adw::EntryRow::builder().title(label).text(value).activates_default(true).build();
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.add_css_class("boxed-list");
        list.append(&entry);
        dialog.set_extra_child(Some(&list));
        dialog.add_response("cancel", tr("Cancel"));
        dialog.add_response("done", tr("Done"));
        dialog.set_response_appearance("done", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("done"));
        dialog.set_close_response("cancel");
        dialog.connect_response(Some("done"), move |_, _| done(entry.text().trim().to_owned()));
        dialog.present(Some(&self.dialog));
    }
}

fn write_lrc(lyrics: &SyncedLyrics) -> String {
    lrc::write(lyrics, lyrics.timing == LyricsTiming::Word)
}

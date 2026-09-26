//! Очередь — панель справа (docs/PROMPT.md §5.2): играющий трек, «Далее», «Далее — похожие»;
//! переход щелчком, удаление кнопкой и клавишей Delete; «Очистить» оставляет играющий.

use adw::prelude::*;
use gtk::glib;
use melogold_playback::engine::{Command, QueueView};

use crate::localization::tr;
use crate::widgets::Cover;
use crate::window::MainWindow;

#[derive(Clone)]
pub struct QueuePanel {
    pub root: adw::ToolbarView,
    list: gtk::ListBox,
    empty: adw::StatusPage,
    stack: gtk::Stack,
}

impl QueuePanel {
    pub fn new(window: &MainWindow) -> QueuePanel {
        let header = adw::HeaderBar::builder().show_end_title_buttons(false).show_start_title_buttons(false).build();
        header.set_title_widget(Some(&adw::WindowTitle::new(tr("QueueTitle"), "")));
        let clear = gtk::Button::builder().icon_name("edit-clear-all-symbolic").tooltip_text(tr("QueueClear")).build();
        let player = window.ctx.services.player.clone();
        let weak = window.downgrade();
        clear.connect_clicked(move |_| {
            player.send(Command::ClearQueue);
            if let Some(window) = weak.upgrade() {
                window.toast(tr("QueueCleared"));
            }
        });
        header.pack_end(&clear);
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.add_css_class("navigation-sidebar");
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&list).vexpand(true).build();
        let empty = adw::StatusPage::builder().icon_name("view-list-bullet-symbolic").title(tr("QueueTitle")).build();
        empty.add_css_class("compact");
        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(&empty, Some("empty"));
        let root = adw::ToolbarView::new();
        root.add_top_bar(&header);
        root.set_content(Some(&stack));
        root.set_width_request(280);
        QueuePanel { root, list, empty, stack }
    }

    pub fn apply(&self, window: &MainWindow, view: &QueueView) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.stack.set_visible_child_name(if view.items.is_empty() { "empty" } else { "list" });
        let _ = &self.empty;
        let player = window.ctx.services.player.clone();
        for (index, item) in view.items.iter().enumerate() {
            if Some(index) == view.autoplay_from {
                let header = gtk::Label::builder().label(tr("QueueSimilar")).xalign(0.0).margin_top(12).margin_start(12).build();
                header.add_css_class("heading");
                header.add_css_class("dim-label");
                let row = gtk::ListBoxRow::builder().child(&header).activatable(false).selectable(false).build();
                self.list.append(&row);
            }
            let current = Some(index) == view.current;
            let cover = Cover::new(40);
            cover.set(&window.ctx.services.images, item.track.thumbnail_url.as_deref(), 120);
            let title = gtk::Label::builder().label(&item.track.title).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
            if current {
                title.add_css_class("accent");
                title.add_css_class("heading");
            }
            let subtitle = gtk::Label::builder().label(item.track.subtitle()).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
            subtitle.add_css_class("dim-label");
            subtitle.add_css_class("caption");
            let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).valign(gtk::Align::Center).build();
            texts.append(&title);
            texts.append(&subtitle);
            let content = gtk::Box::builder().spacing(10).build();
            content.append(&cover.root);
            content.append(&texts);
            if !current {
                let remove = gtk::Button::builder()
                    .icon_name("list-remove-symbolic")
                    .tooltip_text(tr("MenuRemoveFromQueue"))
                    .valign(gtk::Align::Center)
                    .build();
                remove.add_css_class("flat");
                let (player, id) = (player.clone(), item.id);
                remove.connect_clicked(move |_| player.send(Command::Remove(id)));
                content.append(&remove);
            }
            let row = gtk::ListBoxRow::builder().child(&content).build();
            row.update_property(&[gtk::accessible::Property::Label(&format!("{}, {}", item.track.title, item.track.subtitle()))]);
            let (player_click, id) = (player.clone(), item.id);
            let click = gtk::GestureClick::new();
            click.connect_released(move |_, _, _, _| player_click.send(Command::JumpTo(id)));
            row.add_controller(click);
            let keys = gtk::EventControllerKey::new();
            let (player_keys, removable) = (player.clone(), !current);
            keys.connect_key_pressed(move |_, key, _, _| match key {
                gtk::gdk::Key::Delete if removable => {
                    player_keys.send(Command::Remove(id));
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => {
                    player_keys.send(Command::JumpTo(id));
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            });
            row.add_controller(keys);
            self.list.append(&row);
        }
    }
}

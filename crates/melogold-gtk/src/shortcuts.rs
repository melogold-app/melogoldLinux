//! Окно «Сочетания клавиш» — `AdwShortcutsDialog` (docs/PROMPT.md §5.3). Открывается по
//! Ctrl+? и F1, из главного меню и из Настроек. Группы растут вместе со срезами: в окне
//! только то, что уже работает.

use adw::prelude::*;

use crate::localization::tr;

pub fn present(parent: Option<&gtk::Window>) -> adw::ShortcutsDialog {
    let dialog = adw::ShortcutsDialog::new();
    let window = adw::ShortcutsSection::new(Some(tr("ShortcutsGroupWindow")));
    for (title, accelerator) in [
        (tr("ShortcutSearch"), "<Control>f slash"),
        (tr("ShortcutSections"), "<Control>1...<Control>3"),
        (tr("ShortcutSettings"), "<Control>comma"),
        (tr("Back"), "<Alt>Left Escape"),
        (tr("LinuxPrimaryMenuShortcut"), "F10"),
        (tr("ShortcutHelp"), "<Control>question F1"),
        (tr("LinuxCloseWindow"), "<Control>w"),
        (tr("LinuxQuit"), "<Control>q"),
    ] {
        window.add(adw::ShortcutsItem::new(title, accelerator));
    }
    dialog.add(window);
    dialog.present(parent);
    dialog
}

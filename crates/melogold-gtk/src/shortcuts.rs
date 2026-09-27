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
        (tr("ShortcutFullScreen"), "F11"),
        (tr("LinuxPrimaryMenuShortcut"), "F10"),
        (tr("ShortcutHelp"), "<Control>question F1"),
        (tr("LinuxCloseWindow"), "<Control>w"),
        (tr("LinuxQuit"), "<Control>q"),
    ] {
        window.add(adw::ShortcutsItem::new(title, accelerator));
    }
    dialog.add(window);
    let playback = adw::ShortcutsSection::new(Some(tr("ShortcutsGroupPlayback")));
    for (title, accelerator) in [
        (tr("ShortcutPlayPause"), "space"),
        (tr("ShortcutNextPrevious"), "<Control>Right <Control>Left"),
        (tr("ShortcutSeek"), "<Shift>Right <Shift>Left"),
        (tr("ShortcutVolume"), "<Control>Up <Control>Down"),
        (tr("ShortcutMute"), "m <Control>m"),
        (tr("ShortcutShuffle"), "<Control>h"),
        (tr("ShortcutRepeat"), "<Control>t"),
        (tr("ShortcutLike"), "<Control>d"),
        (tr("ShortcutLyrics"), "<Control>l"),
        (tr("ShortcutQueue"), "<Control>u"),
    ] {
        playback.add(adw::ShortcutsItem::new(title, accelerator));
    }
    dialog.add(playback);
    let lists = adw::ShortcutsSection::new(Some(tr("ShortcutsGroupLists")));
    for (title, accelerator) in [
        (tr("ShortcutRowPlay"), "Return"),
        (tr("ShortcutRowMenu"), "Menu <Shift>F10"),
        (tr("ShortcutRowRemove"), "Delete"),
        (tr("ShortcutRowMove"), "<Alt>Up <Alt>Down"),
        (tr("ShortcutSelectAll"), "<Control>a"),
    ] {
        lists.add(adw::ShortcutsItem::new(title, accelerator));
    }
    dialog.add(lists);
    let editor = adw::ShortcutsSection::new(Some(tr("ShortcutsGroupEditor")));
    for (title, accelerator) in [
        (tr("ShortcutMark"), "Return"),
        (tr("ShortcutMarkEnd"), "<Shift>Return"),
        (tr("ShortcutRemark"), "BackSpace"),
        (tr("ShortcutCursor"), "Up Down"),
        (tr("ShortcutEditorSeek"), "Left Right"),
        (tr("ShortcutNudge"), "bracketleft bracketright"),
        (tr("ShortcutUndo"), "<Control>z"),
        (tr("ShortcutSave"), "<Control>s"),
    ] {
        editor.add(adw::ShortcutsItem::new(title, accelerator));
    }
    dialog.add(editor);
    dialog.present(parent);
    dialog
}

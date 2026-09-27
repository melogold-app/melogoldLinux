//! AppImage и рабочий стол (docs/PROMPT.md §3, грабли §9 п. 13): при первом запуске и после
//! обновления `.desktop` с `Exec` на этот AppImage (ссылки `melogold://` и пункт в меню) и значки
//! hicolor ложатся в `~/.local/share`, кэши обновляются — иначе в доке остаётся старый значок.
//! deb, rpm и Flatpak ставят это сами; здесь ничего не делается.

use std::path::{Path, PathBuf};

use melogold_core::app_info::{APP_ID, VERSION};

/// Вызывается при запуске; работа — не в главном потоке.
pub fn ensure(runtime: &tokio::runtime::Handle) {
    let (Some(appimage), Some(appdir)) = (std::env::var_os("APPIMAGE").map(PathBuf::from), std::env::var_os("APPDIR").map(PathBuf::from))
    else {
        return;
    };
    if crate::app::snapshot_mode() {
        return;
    }
    runtime.spawn_blocking(move || {
        if let Err(error) = integrate(&appimage, &appdir) {
            tracing::warn!(%error, "AppImage не встроился в меню");
        }
    });
}

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
}

fn integrate(appimage: &Path, appdir: &Path) -> std::io::Result<()> {
    let share = data_home();
    let source = std::fs::read_to_string(appdir.join(format!("usr/share/applications/{APP_ID}.desktop")))?;
    let exec = format!("\"{}\"", appimage.display());
    let mut desktop: String = source
        .lines()
        .map(|line| {
            if let Some(rest) = line.strip_prefix("Exec=") {
                let arguments = rest.split_once(' ').map(|(_, a)| a).unwrap_or("%U");
                format!("Exec={exec} {arguments}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    desktop.push_str(&format!("\nX-AppImage-Version={VERSION}\n"));
    let target = share.join(format!("applications/{APP_ID}.desktop"));
    let changed = std::fs::read_to_string(&target).ok().as_deref() != Some(desktop.as_str());
    if !changed {
        return Ok(());
    }
    std::fs::create_dir_all(target.parent().expect("папка applications"))?;
    std::fs::write(&target, desktop)?;
    let icons = appdir.join("usr/share/icons/hicolor");
    if let Ok(sizes) = std::fs::read_dir(&icons) {
        for size in sizes.flatten() {
            let icon = size.path().join(format!("apps/{APP_ID}.png"));
            if icon.is_file() {
                let destination = share.join("icons/hicolor").join(size.file_name()).join(format!("apps/{APP_ID}.png"));
                std::fs::create_dir_all(destination.parent().expect("папка значка"))?;
                std::fs::copy(&icon, &destination)?;
            }
        }
    }
    let run = |program: &str, args: &[&Path]| {
        let _ = std::process::Command::new(program)
            .args(args)
            .env_remove("LD_LIBRARY_PATH")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    };
    run("update-desktop-database", &[Path::new("-q"), &share.join("applications")]);
    run("gtk-update-icon-cache", &[Path::new("-q"), Path::new("-f"), Path::new("-t"), &share.join("icons/hicolor")]);
    tracing::info!(appimage = %appimage.display(), "AppImage встроен в меню");
    Ok(())
}

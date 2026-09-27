//! Обновления (docs/PROMPT.md §3 «Обновления», Windows `UpdateService.cs`): проверка при каждом
//! запуске (через 5 с — окно и музыка важнее) и раз в 6 часов; нашлась версия новее — уведомление
//! GNOME, полоса «Вышла новая версия» в Настройках и точка у «Настроек».
//!
//! - AppImage обновляется сам: скачивание с ходом → проверка размера и SHA-256 → новый файл встаёт
//!   на место старого → Melogold перезапускается;
//! - deb, rpm и Flatpak сами себя не обновляют: «Скачать» ведёт на страницу релиза;
//! - отладочная сборка не обновляется совсем.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gio, glib};
use melogold_core::app_info::{self, VERSION};
use melogold_core::settings::keys;
use melogold_core::updates::{self, UpdateAsset, UpdateManifest};
use sha2::{Digest, Sha256};

use crate::app::AppContext;
use crate::localization::{tr, trf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateState {
    /// Отладочная сборка: сама не обновляется.
    Disabled,
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading(u8),
    Installing,
    /// Нет связи с GitHub или файл не прошёл проверку.
    Failed,
}

/// Как эта сборка обновляется.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Disabled,
    /// AppImage по этому пути заменяется новым.
    AppImage(PathBuf),
    /// deb, rpm, Flatpak: ссылка на релиз.
    Package,
}

#[derive(Clone)]
pub struct UpdateService(Rc<Inner>);

pub struct Inner {
    ctx: Rc<AppContext>,
    http: reqwest::Client,
    pub mode: Mode,
    state: RefCell<UpdateState>,
    available: RefCell<Option<UpdateManifest>>,
    started: Cell<bool>,
    listeners: RefCell<Vec<Weak<dyn Fn()>>>,
}

impl std::ops::Deref for UpdateService {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl UpdateService {
    pub fn new(ctx: Rc<AppContext>) -> UpdateService {
        let mode = if cfg!(debug_assertions) && std::env::var_os("MELOGOLD_UPDATES").is_none() {
            Mode::Disabled
        } else if let Some(path) = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file()) {
            Mode::AppImage(path)
        } else {
            Mode::Package
        };
        let http = reqwest::Client::builder()
            .user_agent(app_info::tool_user_agent())
            .connect_timeout(Duration::from_secs(15))
            .build()
            .expect("HTTP-клиент обновлений");
        let state = if mode == Mode::Disabled { UpdateState::Disabled } else { UpdateState::Idle };
        UpdateService(Rc::new(Inner {
            ctx,
            http,
            mode,
            state: RefCell::new(state),
            available: RefCell::default(),
            started: Cell::new(false),
            listeners: RefCell::default(),
        }))
    }

    pub fn state(&self) -> UpdateState {
        self.state.borrow().clone()
    }

    pub fn available(&self) -> Option<UpdateManifest> {
        self.available.borrow().clone()
    }

    /// Снимки окна: полоса «Вышла новая версия» без сети.
    pub fn pretend_available(&self, manifest: UpdateManifest) {
        self.available.replace(Some(manifest));
        self.set(UpdateState::Available);
    }

    /// Экран, который перерисовывается при смене состояния; живёт, пока жив `refresh`.
    pub fn listen(&self, refresh: &Rc<dyn Fn()>) {
        self.listeners.borrow_mut().push(Rc::downgrade(refresh));
    }

    fn set(&self, state: UpdateState) {
        self.state.replace(state);
        let alive: Vec<Rc<dyn Fn()>> = {
            let mut list = self.listeners.borrow_mut();
            list.retain(|l| l.strong_count() > 0);
            list.iter().filter_map(Weak::upgrade).collect()
        };
        for refresh in alive {
            refresh();
        }
    }

    /// Проверки без нажатия: при запуске через 5 с и потом раз в 6 часов, пока Melogold открыт.
    pub fn start(&self, found: impl Fn(&UpdateManifest) + 'static) {
        if self.mode == Mode::Disabled || self.started.replace(true) || !self.ctx.settings.get(&keys::UPDATES_AUTO) {
            return;
        }
        let found: Rc<dyn Fn(&UpdateManifest)> = Rc::new(found);
        let weak = Rc::downgrade(&self.0);
        glib::timeout_add_local_once(Duration::from_secs(5), move || {
            let Some(inner) = weak.upgrade() else { return };
            let this = UpdateService(inner);
            this.check(false, Some(Rc::clone(&found)));
            let weak = Rc::downgrade(&this.0);
            glib::timeout_add_local(Duration::from_millis(updates::CHECK_INTERVAL_MS as u64), move || {
                let Some(inner) = weak.upgrade() else { return glib::ControlFlow::Break };
                UpdateService(inner).check(true, Some(Rc::clone(&found)));
                glib::ControlFlow::Continue
            });
        });
    }

    /// Проверка; без `force` — не чаще раза в 6 часов. `found` — нашлась версия новее.
    pub fn check(&self, force: bool, found: Option<Rc<dyn Fn(&UpdateManifest)>>) {
        if matches!(self.state(), UpdateState::Disabled | UpdateState::Checking | UpdateState::Downloading(_) | UpdateState::Installing) {
            return;
        }
        let now = melogold_core::text::now_ms();
        if !force && now - self.ctx.settings.get(&keys::UPDATES_LAST_CHECK) < updates::CHECK_INTERVAL_MS {
            return;
        }
        self.set(UpdateState::Checking);
        let http = self.http.clone();
        let task = self.ctx.services.run(async move {
            let response = http.get(updates::MANIFEST_URL).timeout(Duration::from_secs(30)).send().await?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            let text = response.error_for_status()?.text().await?;
            Ok::<_, reqwest::Error>(serde_json::from_str::<UpdateManifest>(&text).ok())
        });
        let this = self.clone();
        glib::spawn_future_local(async move {
            match task.await {
                Some(Ok(manifest)) => {
                    this.ctx.settings.set(&keys::UPDATES_LAST_CHECK, now);
                    // AppImage — только если в релизе есть файл этой архитектуры; пакетам хватает страницы релиза.
                    let newer = manifest.filter(|m| {
                        updates::is_newer(&m.version, VERSION) && (!matches!(this.mode, Mode::AppImage(_)) || m.asset().is_some())
                    });
                    this.available.replace(newer.clone());
                    this.set(if newer.is_some() { UpdateState::Available } else { UpdateState::UpToDate });
                    if let (Some(manifest), Some(found)) = (newer, found) {
                        tracing::info!(версия = %manifest.version, "вышла новая версия");
                        found(&manifest);
                    }
                }
                other => {
                    tracing::warn!(?other, "проверка обновлений не прошла");
                    this.set(UpdateState::Failed);
                }
            }
        });
    }

    /// «Обновить»: AppImage — скачать, проверить и заменить; пакеты — страница релиза.
    pub fn install(&self, parent: Option<&gtk::Window>) {
        let Some(manifest) = self.available() else { return };
        let path = match &self.mode {
            Mode::AppImage(path) => path.clone(),
            _ => {
                gtk::UriLauncher::new(updates::RELEASES_URL).launch(parent, gio::Cancellable::NONE, |result| {
                    if let Err(error) = result {
                        tracing::warn!(%error, "страница релиза не открылась");
                    }
                });
                return;
            }
        };
        let Some(asset) = manifest.asset().cloned() else { return };
        if matches!(self.state(), UpdateState::Downloading(_) | UpdateState::Installing) {
            return;
        }
        self.set(UpdateState::Downloading(0));
        let (progress_sender, progress) = async_channel::unbounded::<u8>();
        let (http, url, folder) = (self.http.clone(), manifest.download_url(&asset), self.ctx.paths.updates());
        let task = self.ctx.services.run(async move { download(&http, &url, &asset, &folder, progress_sender).await });
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            while let Ok(percent) = progress.recv().await {
                let Some(inner) = weak.upgrade() else { return };
                UpdateService(inner).set(UpdateState::Downloading(percent));
            }
        });
        let this = self.clone();
        glib::spawn_future_local(async move {
            let downloaded = task.await.and_then(Result::ok);
            let Some(file) = downloaded else {
                tracing::warn!("обновление не скачалось");
                this.set(UpdateState::Failed);
                return;
            };
            this.set(UpdateState::Installing);
            match replace_and_restart(&file, &path) {
                Ok(()) => {
                    tracing::info!(версия = %manifest.version, "обновление установлено, перезапуск");
                    if let Some(app) = gio::Application::default() {
                        app.quit();
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "обновление не встало на место");
                    this.set(UpdateState::Failed);
                }
            }
        });
    }

    /// Строка версии в «О приложении».
    pub fn version_text(&self) -> String {
        match (self.state(), self.available()) {
            (UpdateState::Disabled, _) => trf("UpdateDisabledFormat", &[&VERSION]),
            (UpdateState::Checking, _) => tr("UpdateChecking").to_owned(),
            (UpdateState::UpToDate, _) => trf("UpdateUpToDateFormat", &[&VERSION]),
            (UpdateState::Failed, None) => trf("UpdateOfflineFormat", &[&VERSION]),
            (UpdateState::Downloading(percent), Some(_)) => trf("UpdateDownloadingFormat", &[&percent]),
            (UpdateState::Installing, Some(_)) => tr("UpdateInstalling").to_owned(),
            (UpdateState::Failed, Some(_)) => tr("UpdateFailed").to_owned(),
            (_, Some(manifest)) => trf("UpdateVersionNewFormat", &[&VERSION, &manifest.version]),
            _ => trf("VersionFormat", &[&VERSION]),
        }
    }

    /// Уведомление GNOME о новой версии — один раз на версию.
    pub fn announce(&self, manifest: &UpdateManifest) {
        if self.ctx.settings.get(&keys::UPDATES_ANNOUNCED).as_deref() == Some(manifest.version.as_str()) {
            return;
        }
        self.ctx.settings.set(&keys::UPDATES_ANNOUNCED, Some(manifest.version.clone()));
        let Some(app) = gio::Application::default() else { return };
        let notification = gio::Notification::new(&trf("UpdateAvailableTitle", &[&manifest.version]));
        notification.set_body(Some(tr("UpdateToastText")));
        notification.set_default_action("app.show-update");
        // AppImage обновляется прямо из уведомления; пакетам — «Скачать» на странице релиза.
        let label = if matches!(self.mode, Mode::AppImage(_)) { tr("UpdateAction") } else { tr("MenuDownload") };
        notification.add_button(label, "app.update-install");
        app.send_notification(Some("update"), &notification);
    }
}

/// Скачать в папку обновлений; размер и SHA-256 — до установки: битый или подменённый файл не ставится.
async fn download(
    http: &reqwest::Client,
    url: &str,
    asset: &UpdateAsset,
    folder: &Path,
    progress: async_channel::Sender<u8>,
) -> Result<PathBuf, String> {
    tokio::fs::create_dir_all(folder).await.map_err(|e| e.to_string())?;
    let name = Path::new(&asset.file_name).file_name().ok_or("имя файла")?.to_owned();
    let target = folder.join(&name);
    if let Ok(bytes) = tokio::fs::read(&target).await {
        if bytes.len() as u64 == asset.size_bytes && hex::encode(Sha256::digest(&bytes)) == asset.sha256.to_lowercase() {
            return Ok(target);
        }
    }
    let mut response = http.get(url).send().await.and_then(reqwest::Response::error_for_status).map_err(|e| e.to_string())?;
    let mut data = Vec::with_capacity(asset.size_bytes as usize);
    let mut last = 0u8;
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        data.extend_from_slice(&chunk);
        let percent = (data.len() as u64 * 100 / asset.size_bytes.max(1)).min(100) as u8;
        if percent != last {
            last = percent;
            let _ = progress.try_send(percent);
        }
    }
    if data.len() as u64 != asset.size_bytes {
        return Err(format!("размер {} вместо {}", data.len(), asset.size_bytes));
    }
    if hex::encode(Sha256::digest(&data)) != asset.sha256.to_lowercase() {
        return Err("SHA-256 не совпал".into());
    }
    let part = target.with_extension("part");
    tokio::fs::write(&part, &data).await.map_err(|e| e.to_string())?;
    tokio::fs::rename(&part, &target).await.map_err(|e| e.to_string())?;
    Ok(target)
}

/// Новый AppImage — рядом со старым под временным именем, затем на его место одним переименованием;
/// новый процесс стартует, когда этот уже закрылся (иначе он отдал бы запуск этому экземпляру).
fn replace_and_restart(downloaded: &Path, appimage: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    let folder = appimage.parent().ok_or_else(|| std::io::Error::other("папка AppImage"))?;
    let name = appimage.file_name().ok_or_else(|| std::io::Error::other("имя AppImage"))?.to_string_lossy().into_owned();
    let staged = folder.join(format!(".{name}.new"));
    std::fs::copy(downloaded, &staged)?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&staged, appimage)?;
    let _ = std::fs::remove_file(downloaded);
    std::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 1; exec \"$0\"")
        .arg(appimage)
        .env_remove("APPIMAGE")
        .env_remove("APPDIR")
        .env_remove("ARGV0")
        .env_remove("OWD")
        .process_group(0)
        .spawn()?;
    Ok(())
}

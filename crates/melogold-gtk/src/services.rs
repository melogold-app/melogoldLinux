//! Всё, что работает не в главном потоке: рантайм tokio, InnerTube, плеер, кэш музыки.
//!
//! Главный поток GTK сеть и диск не ждёт никогда (docs/PROMPT.md §3): работа уходит в рантайм
//! tokio ([`Services::run`]), а окно дожидается результата как обычного future в цикле GLib.

use std::future::Future;
use std::sync::Arc;

use melogold_core::app_info;
use melogold_core::paths::AppPaths;
use melogold_core::settings::keys;
use melogold_data::{Change, Database, Library};
use melogold_innertube::lyrics::{Community, KuGou, LrcLib, LyricsFetcher, ProviderError};
use melogold_innertube::music::YouTubeMusic;
use melogold_innertube::{locale_from, InnerTube};
use melogold_playback::downloads::Downloads;
use melogold_playback::engine::{self, PlayerHandle};
use melogold_playback::resolver::Resolver;
use melogold_playback::song_cache::SongCache;
use melogold_playback::stream_clients;
use melogold_server::account::{Account, AccountState, DeviceIdentity};
use melogold_server::session::SessionStore;
use melogold_server::sync::{LibrarySync, SyncStatus};

use crate::images::Images;
use crate::localization::{self, Lang};
use crate::settings_store::SettingsStore;

pub struct Services {
    runtime: tokio::runtime::Runtime,
    pub music: YouTubeMusic,
    pub resolver: Arc<Resolver>,
    pub songs: Arc<SongCache>,
    pub player: PlayerHandle,
    pub images: Images,
    pub library: Arc<Library>,
    pub downloads: Arc<Downloads>,
    /// Изменения библиотеки — в главный поток: экраны обновляются сами.
    pub library_changes: async_channel::Receiver<Change>,
    /// У трека изменилась загрузка или кэш — метки «есть без сети».
    pub offline_changes: async_channel::Receiver<String>,
    /// Аккаунт и синхронизация (срез 5).
    pub account: Arc<Account>,
    pub sync: Arc<LibrarySync>,
    pub account_changes: async_channel::Receiver<AccountState>,
    pub sync_status: async_channel::Receiver<SyncStatus>,
    /// Список устройств изменился на сервере (`devices.updated`).
    pub devices_changes: async_channel::Receiver<()>,
    /// Цепочка поиска текстов (срез 6): YouTube Music, LrcLib, KuGou, затем сервер Melogold.
    pub lyrics: Arc<LyricsFetcher>,
    /// Свой текст длиннее лимита сервера: он остался только здесь.
    pub lyrics_rejected: async_channel::Receiver<String>,
}

impl Services {
    pub fn start(paths: &AppPaths, settings: &SettingsStore) -> Services {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(3)
            .thread_name("melogold-worker")
            .enable_all()
            .build()
            .expect("рантайм tokio запускается");
        if let Err(error) = gst::init() {
            tracing::error!(%error, "GStreamer не запустился");
        }

        // Язык контента — по языку системы (грабли §9 п. 8), а выбранный язык приложения — главнее.
        let system =
            std::env::var("LC_ALL").ok().filter(|v| !v.is_empty()).or_else(|| std::env::var("LANG").ok()).unwrap_or_else(|| "en_US".into());
        let (mut hl, gl) = locale_from(&system);
        if localization::is_forced() {
            hl = if localization::lang() == Lang::Ru { "ru".into() } else { "en".into() };
        }
        let client = InnerTube::new(&hl, &gl);
        let music = YouTubeMusic::new(client.clone());
        let clients_path = paths.stream_clients();
        let resolver = Arc::new(Resolver::new(client.clone(), stream_clients::load_saved(&clients_path)));
        let limit_mb = settings.get(&keys::STREAM_CACHE_MB);
        let songs = SongCache::new(paths.song_cache(), if limit_mb <= 0 { 0 } else { limit_mb * 1024 * 1024 });
        let database = match Database::open(&paths.database()) {
            Ok(database) => database,
            Err(error) => {
                // Не открылась база — работаем с пустой в памяти, а не падаем: музыка важнее.
                tracing::error!(%error, "библиотека не открылась");
                Database::in_memory().expect("база в памяти открывается")
            }
        };
        let library = Library::open(database);
        let (change_sender, library_changes) = async_channel::unbounded();
        library.subscribe(move |change| {
            let _ = change_sender.try_send(change);
        });
        let (offline_sender, offline_changes) = async_channel::unbounded();
        let sender = offline_sender.clone();
        songs.set_listener(move |video_id| {
            let _ = sender.try_send(video_id.to_owned());
        });
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .pool_idle_timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("HTTP-клиент собирается");
        let downloads = Downloads::new(
            SongCache::new(paths.downloads(), 0),
            Arc::clone(&songs),
            Arc::clone(&resolver),
            Arc::clone(&library),
            http.clone(),
            runtime.handle().clone(),
        );
        downloads.set_listener(move |video_id| {
            let _ = offline_sender.try_send(video_id.to_owned());
        });
        let player = engine::start(
            runtime.handle(),
            engine::Deps {
                resolver: Arc::clone(&resolver),
                music: music.clone(),
                songs: Arc::clone(&songs),
                downloads: Some(Arc::clone(downloads.store())),
                library: Some(Arc::clone(&library)),
                http: http.clone(),
                settings: playback_settings(settings),
                queue_path: Some(paths.data().join("queue.json")),
            },
        );
        let images = Images::new(paths.images(), http.clone(), runtime.handle().clone());

        // В фоне: visitorData (без него первый трек ждал бы лишний запрос) и свежий список клиентов потока.
        {
            let (client, resolver, http) = (client.clone(), Arc::clone(&resolver), http.clone());
            runtime.spawn(async move {
                if let Err(error) = client.ensure_visitor_data().await {
                    tracing::info!(%error, "visitorData не получен заранее");
                }
                if let Some(fresh) = stream_clients::refresh(&http, &clients_path, &app_info::tool_user_agent()).await {
                    resolver.set_clients(fresh);
                }
            });
        }
        {
            let _guard = runtime.enter();
            downloads.resume();
        }
        tracing::info!(hl = %hl, gl = %gl, "InnerTube");

        // Аккаунт: сессия — из связки ключей. Снимки окна связку пользователя не трогают.
        let identity = DeviceIdentity {
            platform_id: melogold_core::hwid::platform_id(paths),
            name: melogold_core::system::device_name(),
            os_version: Some(melogold_core::system::os_pretty_name()),
            model: melogold_core::system::product_name(),
            client_version: app_info::VERSION.to_owned(),
            language: if localization::lang() == Lang::Ru { "ru".into() } else { "en".into() },
        };
        let snapshots = cfg!(debug_assertions) && std::env::var_os("MELOGOLD_SCREENSHOT_DIR").is_some();
        let store = if snapshots { SessionStore::file_only(paths.session_fallback()) } else { SessionStore::new(paths.session_fallback()) };
        let server_url = settings.get(&keys::SERVER_URL);
        let account = Account::new(identity, store, server_url);
        let (account_sender, account_changes) = async_channel::unbounded();
        account.subscribe(move |state| {
            let _ = account_sender.try_send(state.clone());
        });
        let sync = LibrarySync::new(Arc::clone(&account), Arc::clone(&library), runtime.handle().clone());
        let (status_sender, sync_status) = async_channel::unbounded();
        sync.subscribe_status(move |status| {
            let _ = status_sender.try_send(status.clone());
        });
        let (devices_sender, devices_changes) = async_channel::unbounded();
        sync.subscribe_devices(move || {
            let _ = devices_sender.try_send(());
        });
        let (rejected_sender, lyrics_rejected) = async_channel::unbounded();
        sync.subscribe_lyrics_rejected(move |video_id| {
            let _ = rejected_sender.try_send(video_id.to_owned());
        });
        {
            let _guard = runtime.enter();
            sync.start();
        }
        let community: Community = {
            let sync = Arc::clone(&sync);
            Arc::new(move |video_id: String| {
                let sync = Arc::clone(&sync);
                Box::pin(async move {
                    sync.lookup_lyrics(&video_id).await.map_err(|error| {
                        if error.is_network() {
                            ProviderError::Network(error.to_string())
                        } else {
                            ProviderError::Other(error.to_string())
                        }
                    })
                })
            })
        };
        let lyrics = Arc::new(LyricsFetcher {
            music: music.clone(),
            lrclib: LrcLib::new(&app_info::tool_user_agent()),
            kugou: KuGou::new(),
            community: Some(community),
        });
        // Сессия — в фоне: связка ключей может отвечать не сразу, окно её не ждёт. Прочитанная
        // сессия запускает синхронизацию через подписку.
        let loading = Arc::clone(&account);
        runtime.spawn(async move { loading.load().await });
        Services {
            runtime,
            music,
            resolver,
            songs,
            player,
            images,
            library,
            downloads,
            library_changes,
            offline_changes,
            account,
            sync,
            account_changes,
            sync_status,
            devices_changes,
            lyrics,
            lyrics_rejected,
        }
    }

    /// Выполнить в рантайме tokio; результат ждётся из главного потока как future.
    pub fn run<F, T>(&self, future: F) -> impl Future<Output = Option<T>> + 'static
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let task = self.runtime.spawn(future);
        async move { task.await.ok() }
    }

    /// Библиотека не в главном потоке: SQLite ждёт диск.
    pub fn db<F, T>(&self, work: F) -> impl Future<Output = Option<T>> + 'static
    where
        F: FnOnce(&Library) -> T + Send + 'static,
        T: Send + 'static,
    {
        let library = Arc::clone(&self.library);
        let task = self.runtime.spawn_blocking(move || work(&library));
        async move { task.await.ok() }
    }

    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// Выход: плеер сохраняет очередь и позицию и останавливается.
    pub fn shutdown(&self) {
        // Неотправленные правки — одной синхронизацией, не дольше 3 с (Windows `Flush`).
        let sync = Arc::clone(&self.sync);
        self.runtime.block_on(async move { sync.flush(std::time::Duration::from_secs(3)).await });
        self.player.send(engine::Command::Shutdown);
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

pub fn playback_settings(settings: &SettingsStore) -> engine::Settings {
    engine::Settings {
        volume: settings.get(&keys::VOLUME).clamp(0.0, 1.0),
        muted: settings.get(&keys::MUTED),
        speed: settings.get(&keys::SPEED).clamp(0.5, 2.0),
        normalize: settings.get(&keys::NORMALIZATION),
        autoplay: settings.get(&keys::AUTOPLAY),
        history_paused: settings.get(&keys::HISTORY_PAUSED),
    }
}

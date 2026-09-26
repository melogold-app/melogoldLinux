//! Всё, что работает не в главном потоке: рантайм tokio, InnerTube, плеер, кэш музыки.
//!
//! Главный поток GTK сеть и диск не ждёт никогда (docs/PROMPT.md §3): работа уходит в рантайм
//! tokio ([`Services::run`]), а окно дожидается результата как обычного future в цикле GLib.

use std::future::Future;
use std::sync::Arc;

use melogold_core::app_info;
use melogold_core::paths::AppPaths;
use melogold_core::settings::keys;
use melogold_innertube::music::YouTubeMusic;
use melogold_innertube::{locale_from, InnerTube};
use melogold_playback::engine::{self, PlayerHandle};
use melogold_playback::resolver::Resolver;
use melogold_playback::song_cache::SongCache;
use melogold_playback::stream_clients;

use crate::images::Images;
use crate::localization::{self, Lang};
use crate::settings_store::SettingsStore;

pub struct Services {
    runtime: tokio::runtime::Runtime,
    pub music: YouTubeMusic,
    pub resolver: Arc<Resolver>,
    #[allow(dead_code)] // «Хранилище» — срез 4
    pub songs: Arc<SongCache>,
    pub player: PlayerHandle,
    pub images: Images,
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
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .pool_idle_timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("HTTP-клиент собирается");
        let player = engine::start(
            runtime.handle(),
            engine::Deps {
                resolver: Arc::clone(&resolver),
                music: music.clone(),
                songs: Arc::clone(&songs),
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
        tracing::info!(hl = %hl, gl = %gl, "InnerTube");
        Services { runtime, music, resolver, songs, player, images }
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

    pub fn handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// Выход: плеер сохраняет очередь и позицию и останавливается.
    pub fn shutdown(&self) {
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
    }
}

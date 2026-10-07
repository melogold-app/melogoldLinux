//! Управление другим устройством (задание 0011): состояние и команды живут здесь, без виджетов.
//!
//! * [`RemoteHub`] связывает плеер, живые события сервера (`playback.updated`, `playback.command`) и
//!   логику `melogold_server::remote`: докладывает, что играет это устройство, ведёт пульт другого,
//!   принимает команды;
//! * [`redirect`] — пока это устройство пульт, команды плееру уходят на управляемое устройство;
//! * [`engine_commands`] — что `playback.command` просит от плеера этого устройства.
//!
//! Окно слушает каналы хаба в главном потоке (`remote_bar.rs`, `remote_sheet.rs`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use melogold_core::music::Track;
use melogold_playback::engine::{Command, Event, PlayerHandle, QueueView, Status};
use melogold_server::account::Account;
use melogold_server::dto::{PlaybackCommandPayload, PlaybackUpdatedPayload};
use melogold_server::remote::{
    incoming_of, should_give_way, AccountRemotePort, AccountReporterPort, Incoming, NoticeThrottle, PlayerSnapshot, RemoteControl,
    RemoteNotice, RemotePort, RemoteView, Reporter, ServerClock,
};
use melogold_server::sync::LibrarySync;
use tokio::runtime::Handle;
use tokio::time::Instant;

type SnapshotFn = Box<dyn Fn() -> Option<PlayerSnapshot> + Send + Sync>;

pub struct RemoteHub {
    pub control: RemoteControl<AccountRemotePort>,
    pub reporter: Reporter<AccountReporterPort<SnapshotFn>>,
    pub clock: Arc<ServerClock>,
    /// Команды другого устройства этому — в главный поток, где живёт громкость и окно.
    pub incoming: async_channel::Receiver<Incoming>,
    pub notices: async_channel::Receiver<RemoteNotice>,
    pub views: async_channel::Receiver<RemoteView>,
    /// «Управляет «Pixel 7 Pro»»: имя устройства, не чаще раза в 30 с.
    pub controlled_by: async_channel::Receiver<String>,
}

impl RemoteHub {
    pub fn start(runtime: &Handle, account: &Arc<Account>, sync: &Arc<LibrarySync>, player: &PlayerHandle) -> Arc<RemoteHub> {
        let clock = Arc::new(ServerClock::default());
        let queue: Arc<Mutex<QueueView>> = Arc::default();

        let snapshot: SnapshotFn = {
            let (player, queue) = (player.clone(), Arc::clone(&queue));
            Box::new(move || {
                let view = queue.lock().ok()?.clone();
                let index = view.current?;
                let state = player.state();
                Some(PlayerSnapshot {
                    tracks: view.items.iter().map(|item| item.track.clone()).collect(),
                    index,
                    position_ms: player.position().map(|p| p.as_millis() as i64).unwrap_or(0),
                    duration_ms: state.duration.map(|d| d.as_millis() as i64),
                    playing: state.playing,
                    volume: Some((state.volume.clamp(0.0, 1.0) * 100.0).round() as u8),
                })
            })
        };
        let reporter = {
            let port = AccountReporterPort { account: Arc::clone(account), clock: Arc::clone(&clock), snapshot };
            // Сессию забрало другое устройство («Слушать здесь»): этому — пауза.
            let player = player.clone();
            let _guard = runtime.enter();
            Reporter::new(port, runtime.clone(), move |_current| player.send_local(Command::Pause))
        };
        let control = {
            let _guard = runtime.enter();
            RemoteControl::new(
                AccountRemotePort { account: Arc::clone(account), clock: Arc::clone(&clock) },
                Arc::clone(&clock),
                runtime.clone(),
            )
        };

        let (incoming_tx, incoming) = async_channel::unbounded();
        let (notice_tx, notices) = async_channel::unbounded();
        let (view_tx, views) = async_channel::unbounded();
        let (controlled_tx, controlled_by) = async_channel::unbounded();

        control.subscribe_notices(move |notice| {
            let _ = notice_tx.try_send(notice.clone());
        });
        // Пока это устройство управляет другим, своё состояние оно не шлёт: иначе затрёт чужое (`newer_state`).
        {
            let reporter = reporter.clone();
            control.subscribe(move |view| {
                reporter.set_paused(view.target.is_some());
                let _ = view_tx.try_send(view.clone());
            });
        }

        // Плеер → докладчик.
        {
            let (events, reporter, queue) = (player.subscribe(), reporter.clone(), Arc::clone(&queue));
            runtime.spawn(async move {
                let mut last: Option<(Option<String>, bool, u8)> = None;
                while let Ok(event) = events.recv().await {
                    match event {
                        Event::State(state) => {
                            if state.status == Status::Playing {
                                reporter.sound_played();
                            }
                            let key =
                                (state.track.as_ref().map(|t| t.video_id.clone()), state.playing, (state.volume * 100.0).round() as u8);
                            if last.as_ref() != Some(&key) {
                                last = Some(key);
                                reporter.changed();
                            }
                        }
                        Event::Queue(view) => {
                            if let Ok(mut slot) = queue.lock() {
                                *slot = view;
                            }
                            reporter.changed();
                        }
                        Event::Seeked(_) => reporter.changed(),
                        _ => {}
                    }
                }
            });
        }

        // Живые события сервера → пульт и исполнитель команд.
        {
            let (control, reporter, clock, account, player) =
                (control.clone(), reporter.clone(), Arc::clone(&clock), Arc::clone(account), player.clone());
            let throttle = NoticeThrottle::default();
            let handle = runtime.clone();
            sync.subscribe_events(move |event| match event.kind.as_str() {
                "system.connected" => {
                    // После (пере)подключения — прочитать состояние заново (§6): пульт мог отстать.
                    let control = control.clone();
                    handle.spawn(async move { control.refresh().await });
                }
                "playback.updated" => {
                    let Ok(payload) = serde_json::from_value::<PlaybackUpdatedPayload>(event.payload.clone()) else { return };
                    clock.update(&event.at, melogold_core::text::now_ms());
                    control.on_updated(payload.cleared, payload.state.as_ref());
                    if let Some(state) = payload.state.as_ref().filter(|_| !payload.cleared) {
                        let me = account.session().map(|s| s.device_id);
                        if should_give_way(state.handoff_from.as_ref(), me.as_deref(), &reporter.session_id(), clock.now()) {
                            // Автопауза (DESIGN §3.12.6): другое устройство забрало воспроизведение себе.
                            player.send_local(Command::Pause);
                        }
                    }
                }
                "playback.command" => {
                    let Ok(payload) = serde_json::from_value::<PlaybackCommandPayload>(event.payload.clone()) else { return };
                    let Some(incoming) = incoming_of(&payload) else { return };
                    let _ = incoming_tx.try_send(incoming);
                    if let Some(name) = payload.from_device_name.filter(|_| throttle.allow(Instant::now())) {
                        let _ = controlled_tx.try_send(name);
                    }
                }
                _ => {}
            });
        }

        Arc::new(RemoteHub { control, reporter, clock, incoming, notices, views, controlled_by })
    }
}

// ── команды плееру, пока это устройство — пульт ──

/// Пока пульт включён, команда плееру уходит на управляемое устройство. `true` — команду забрал пульт.
/// Своим остаются выход, сохранение очереди и настройки (скорость, нормализация).
pub fn redirect<P: RemotePort>(control: &RemoteControl<P>, command: &Command) -> bool {
    if !control.active() {
        return false;
    }
    match command {
        Command::PlayList { tracks, start, shuffle } => {
            if *shuffle {
                let (list, first) = shuffled(tracks, *start);
                control.play_queue(&list, first);
            } else {
                control.play_queue(tracks, *start);
            }
        }
        // Одиночный трек: очередь из одного трека (дальше «похожие» — дело плеера, у пульта такой команды нет).
        Command::PlaySingle { track, .. } => {
            control.play_queue(std::slice::from_ref(track), 0);
        }
        Command::PlayListAt { tracks, start, .. } => {
            control.play_queue(tracks, *start);
        }
        Command::TogglePlay => control.toggle(),
        Command::Play => control.play(),
        Command::Pause => control.pause(),
        Command::Next => control.next(),
        Command::Previous => control.previous(),
        Command::Seek(to) => control.seek_to(to.as_millis() as i64),
        Command::SeekBy(delta_ms) => {
            let now = control.view().now.map(|n| n.position_at(control.clock().now())).unwrap_or(0);
            control.seek_to((now + delta_ms).max(0));
        }
        // У пульта нет таких действий: очередь и повтор — на самом устройстве.
        Command::PlayNext(_)
        | Command::AddToEnd(_)
        | Command::JumpTo(_)
        | Command::Remove(_)
        | Command::Move { .. }
        | Command::ClearQueue
        | Command::SetRepeat(_)
        | Command::SetShuffle(_)
        | Command::Retry
        | Command::RestoreQueue(_)
        | Command::SetSleepTimer(_)
        | Command::SleepAtTrackEnd
        | Command::CancelSleepTimer => {}
        _ => return false,
    }
    true
}

/// Перемешанный список, в котором выбранный трек стоит первым.
fn shuffled(tracks: &[Track], first: usize) -> (Vec<Track>, usize) {
    let mut rest: Vec<Track> = tracks.iter().enumerate().filter(|(i, _)| *i != first).map(|(_, t)| t.clone()).collect();
    let mut random = vec![0u8; rest.len() * 4];
    melogold_core::ids::fill_random(&mut random);
    for i in (1..rest.len()).rev() {
        let r = u32::from_le_bytes([random[i * 4], random[i * 4 + 1], random[i * 4 + 2], random[i * 4 + 3]]) as usize;
        rest.swap(i, r % (i + 1));
    }
    let mut list = Vec::with_capacity(tracks.len());
    if let Some(track) = tracks.get(first) {
        list.push(track.clone());
    }
    list.extend(rest);
    (list, 0)
}

// ── команды другого устройства этому ──

/// Что `playback.command` просит от плеера этого устройства. Громкость окно применяет само (ползунок,
/// настройки, плеер): здесь для неё пусто.
pub fn engine_commands(incoming: &Incoming) -> Vec<Command> {
    match incoming {
        Incoming::Play => vec![Command::Play],
        Incoming::Pause | Incoming::Stop => vec![Command::Pause],
        Incoming::Toggle => vec![Command::TogglePlay],
        Incoming::Next => vec![Command::Next],
        Incoming::Previous => vec![Command::Previous],
        Incoming::Seek(ms) => vec![Command::Seek(Duration::from_millis((*ms).max(0) as u64))],
        Incoming::Volume(_) => vec![],
        Incoming::PlayQueue { tracks, index, position_ms } => {
            let mut commands = vec![Command::PlayList { tracks: tracks.clone(), start: *index, shuffle: false }];
            // Перенос с другого устройства: перемотка до того, как трек открылся, — он начнётся с этой секунды.
            if let Some(ms) = position_ms.filter(|ms| *ms > 0) {
                commands.push(Command::Seek(Duration::from_millis(ms as u64)));
            }
            commands
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use melogold_server::api::ApiError;
    use melogold_server::dto::{
        PlaybackStateResponse, PlaybackSummary, RemoteCommand, RemoteCommandResult, RemoteDevice, RemoteDeviceList,
    };

    use super::*;

    #[derive(Default)]
    struct Sent(Mutex<Vec<RemoteCommand>>);

    struct Port(Arc<Sent>);

    impl RemotePort for Port {
        async fn devices(&self) -> Result<RemoteDeviceList, ApiError> {
            Ok(RemoteDeviceList::default())
        }

        async fn playback_state(&self) -> Result<PlaybackStateResponse, ApiError> {
            Ok(PlaybackStateResponse::default())
        }

        async fn send(&self, command: RemoteCommand) -> Result<RemoteCommandResult, ApiError> {
            self.0 .0.lock().unwrap().push(command);
            Ok(RemoteCommandResult { delivered: true })
        }
    }

    fn track(i: usize) -> Track {
        Track { video_id: format!("{i:011}"), title: format!("Трек {i}"), ..Default::default() }
    }

    fn control(sent: &Arc<Sent>) -> RemoteControl<Port> {
        RemoteControl::new(Port(Arc::clone(sent)), Arc::new(ServerClock::default()), Handle::current())
    }

    fn device() -> RemoteDevice {
        RemoteDevice {
            device_id: "mac".into(),
            name: "MacBook Air".into(),
            platform: "macos".into(),
            online: true,
            controllable: true,
            playing: Some(PlaybackSummary { device_id: "mac".into(), playing: true, duration_ms: Some(200_000), ..Default::default() }),
            volume: Some(50),
        }
    }

    async fn settle() {
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn own_player_keeps_its_commands_while_no_device_is_chosen() {
        let sent = Arc::new(Sent::default());
        let control = control(&sent);
        assert!(!redirect(&control, &Command::TogglePlay));
        assert!(!redirect(&control, &Command::PlayList { tracks: vec![track(1)], start: 0, shuffle: false }));
        settle().await;
        assert!(sent.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_remote_takes_the_commands_of_the_player() {
        let sent = Arc::new(Sent::default());
        let control = control(&sent);
        control.connect(&device());
        settle().await;
        for command in [
            Command::TogglePlay,
            Command::Play,
            Command::Pause,
            Command::Next,
            Command::Previous,
            Command::Seek(Duration::from_secs(90)),
            Command::PlayList { tracks: (0..10).map(track).collect(), start: 4, shuffle: false },
            Command::PlaySingle { track: track(7), start: Duration::ZERO },
        ] {
            assert!(redirect(&control, &command), "{command:?}");
        }
        settle().await;
        let commands = sent.0.lock().unwrap().clone();
        let actions: Vec<&str> = commands.iter().map(|c| c.action.as_str()).collect();
        assert_eq!(actions, ["toggle", "play", "pause", "next", "previous", "seek", "play_queue", "play_queue"]);
        assert_eq!(commands[5].position_ms, Some(90_000));
        assert_eq!((commands[6].queue.as_ref().map(Vec::len), commands[6].index), (Some(10), Some(4)), "очередь списка и индекс трека");
        assert_eq!((commands[7].queue.as_ref().map(Vec::len), commands[7].index), (Some(1), Some(0)));
    }

    #[tokio::test]
    async fn shuffled_list_starts_with_the_chosen_track() {
        let sent = Arc::new(Sent::default());
        let control = control(&sent);
        control.connect(&device());
        assert!(redirect(&control, &Command::PlayList { tracks: (0..30).map(track).collect(), start: 12, shuffle: true }));
        settle().await;
        let command = sent.0.lock().unwrap().last().unwrap().clone();
        let queue = command.queue.unwrap();
        assert_eq!((queue.len(), command.index), (30, Some(0)));
        assert_eq!(queue[0].video_id, format!("{:011}", 12));
        let mut ids: Vec<String> = queue.iter().map(|t| t.video_id.clone()).collect();
        ids.sort();
        assert_eq!(ids, (0..30).map(|i| format!("{i:011}")).collect::<Vec<_>>(), "все треки на месте");
    }

    #[tokio::test]
    async fn quit_save_and_settings_stay_with_this_player_and_queue_edits_are_ignored() {
        let sent = Arc::new(Sent::default());
        let control = control(&sent);
        control.connect(&device());
        assert!(!redirect(&control, &Command::Shutdown));
        assert!(!redirect(&control, &Command::SaveQueue));
        assert!(!redirect(&control, &Command::Settings(melogold_playback::engine::Settings::default())));
        // Очередь и повтор — на самом устройстве: команда забрана и ничего не шлёт.
        assert!(redirect(&control, &Command::ClearQueue));
        assert!(redirect(&control, &Command::AddToEnd(vec![track(1)])));
        assert!(redirect(&control, &Command::SetShuffle(true)));
        settle().await;
        assert!(sent.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn seek_by_counts_from_where_the_remote_is() {
        let sent = Arc::new(Sent::default());
        let control = control(&sent);
        let mut playing = device();
        if let Some(p) = playing.playing.as_mut() {
            p.position_ms = 60_000;
            p.playing = false;
            p.at = "2026-09-30T10:00:00.000Z".into();
        }
        control.connect(&playing);
        assert!(redirect(&control, &Command::SeekBy(10_000)));
        assert!(redirect(&control, &Command::SeekBy(-100_000)));
        settle().await;
        let commands = sent.0.lock().unwrap().clone();
        assert_eq!(commands[0].position_ms, Some(70_000));
        assert_eq!(commands[1].position_ms, Some(0), "не раньше начала");
    }

    #[test]
    fn every_incoming_command_reaches_the_player() {
        assert_eq!(engine_commands(&Incoming::Play).len(), 1);
        assert!(matches!(engine_commands(&Incoming::Play)[0], Command::Play));
        assert!(matches!(engine_commands(&Incoming::Pause)[0], Command::Pause));
        assert!(matches!(engine_commands(&Incoming::Stop)[0], Command::Pause));
        assert!(matches!(engine_commands(&Incoming::Toggle)[0], Command::TogglePlay));
        assert!(matches!(engine_commands(&Incoming::Next)[0], Command::Next));
        assert!(matches!(engine_commands(&Incoming::Previous)[0], Command::Previous));
        assert!(matches!(engine_commands(&Incoming::Seek(83_000))[0], Command::Seek(d) if d == Duration::from_secs(83)));
        assert!(engine_commands(&Incoming::Volume(30)).is_empty(), "громкость применяет окно");
        let handoff = engine_commands(&Incoming::PlayQueue { tracks: (0..5).map(track).collect(), index: 3, position_ms: Some(83_000) });
        assert!(matches!(handoff.get(1), Some(Command::Seek(d)) if *d == Duration::from_secs(83)), "перенос — с той же секунды");
        match &engine_commands(&Incoming::PlayQueue { tracks: (0..5).map(track).collect(), index: 3, position_ms: None })[0] {
            Command::PlayList { tracks, start, shuffle } => assert_eq!((tracks.len(), *start, *shuffle), (5, 3, false)),
            other => panic!("{other:?}"),
        }
    }
}

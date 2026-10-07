//! Пульт против настоящего сервера (задание 0011): два устройства одного аккаунта, состояние, команды,
//! «Слушать здесь», выключенное управление, устройство не в сети.
//!
//! ```sh
//! MELOGOLD_LIVE_SERVER=http://127.0.0.1:18080 cargo test -p melogold-server --test live_remote -- --ignored --test-threads=1 --nocapture
//! ```

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{link_second, next_event, open_events, pair, wait_for};
use melogold_core::music::Track;
use melogold_server::dto::{PlaybackCommandPayload, PlaybackHandoffInput, PlaybackUpdatedPayload};
use melogold_server::remote::*;

fn tracks() -> Vec<Track> {
    [
        ("dQw4w9WgXcQ", "Never Gonna Give You Up", "Rick Astley"),
        ("fJ9rUzIMcZQ", "Bohemian Rhapsody", "Queen"),
        ("cYKAr38pZcY", "Photosynthesis", "Saba"),
    ]
    .into_iter()
    .map(|(id, title, artist)| Track {
        video_id: id.into(),
        title: title.into(),
        artists_text: Some(artist.into()),
        duration_ms: Some(213_000),
        ..Default::default()
    })
    .collect()
}

struct Port {
    shot: Mutex<PlayerSnapshot>,
    account: Arc<melogold_server::account::Account>,
    clock: Arc<ServerClock>,
}

impl ReporterPort for Port {
    async fn snapshot(&self) -> Option<PlayerSnapshot> {
        Some(self.shot.lock().unwrap().clone())
    }

    async fn send(&self, put: melogold_server::dto::PlaybackPut) -> PutOutcome {
        let result = self.account.put_playback(put).await;
        if let Ok(ok) = &result {
            self.clock.update(&ok.server_time, melogold_core::text::now_ms());
        }
        put_outcome(result)
    }

    fn server_now_ms(&self) -> i64 {
        self.clock.now()
    }
}

#[tokio::test]
#[ignore = "нужен локальный сервер: MELOGOLD_LIVE_SERVER"]
async fn phone_controls_the_computer_and_back() {
    let pair = pair().await;
    link_second(&pair).await;
    assert!(pair.first.ensure_server_info().await.unwrap().features.remote.is_some(), "сервер без features.remote");
    let first_id = pair.first.session().unwrap().device_id;
    let handle = tokio::runtime::Handle::current();

    // Компьютер: слушает события с remote=1 и сообщает, что играет.
    let (computer_stream, mut computer_events) = open_events(&pair.first, true);
    next_event(&mut computer_events, "system.connected").await;
    let clock = Arc::new(ServerClock::default());
    let port = Port {
        shot: Mutex::new(PlayerSnapshot {
            tracks: tracks(),
            index: 0,
            position_ms: 83_000,
            duration_ms: Some(213_000),
            playing: true,
            volume: Some(60),
        }),
        account: Arc::clone(&pair.first),
        clock: Arc::clone(&clock),
    };
    let reporter = Reporter::new(port, handle.clone(), |_| {});

    // Телефон: события без remote, пульт.
    let (phone_stream, mut phone_events) = open_events(&pair.second, false);
    next_event(&mut phone_events, "system.connected").await;
    reporter.sound_played();
    let update = next_event(&mut phone_events, "playback.updated").await;
    let payload: PlaybackUpdatedPayload = serde_json::from_value(update.payload).unwrap();
    let summary = payload.state.expect("состояние");
    assert_eq!(summary.device_id, first_id);
    assert_eq!(summary.track.as_ref().map(|t| t.title.as_str()), Some("Never Gonna Give You Up"));
    assert_eq!((summary.position_ms, summary.playing, summary.volume), (83_000, true, Some(60)));

    let phone_clock = Arc::new(ServerClock::default());
    let control = RemoteControl::new(
        AccountRemotePort { account: Arc::clone(&pair.second), clock: Arc::clone(&phone_clock) },
        Arc::clone(&phone_clock),
        handle.clone(),
    );
    let devices = control.devices().await.unwrap();
    let computer = devices.iter().find(|d| d.device_id == first_id).expect("компьютер в списке");
    assert!(computer.online && computer.controllable);
    assert_eq!(computer.playing.as_ref().map(|p| p.playing), Some(true));
    assert_eq!(computer.volume, Some(60));
    control.connect(computer);
    wait_for(|| control.view().now.map(|_| ())).await;

    // Каждая команда доходит до компьютера быстро и с нужными полями.
    let mut latency = Vec::new();
    for (send, expected) in [
        (Box::new(|c: &RemoteControl<AccountRemotePort>| c.pause()) as Box<dyn Fn(&RemoteControl<AccountRemotePort>)>, Incoming::Pause),
        (Box::new(|c| c.play()), Incoming::Play),
        (Box::new(|c| c.toggle()), Incoming::Toggle),
        (Box::new(|c| c.next()), Incoming::Next),
        (Box::new(|c| c.previous()), Incoming::Previous),
        (Box::new(|c| c.seek_to(120_000)), Incoming::Seek(120_000)),
        (Box::new(|c| c.set_volume(35)), Incoming::Volume(35)),
        (Box::new(|c| c.stop()), Incoming::Stop),
    ] {
        let started = Instant::now();
        send(&control);
        let event = next_event(&mut computer_events, "playback.command").await;
        latency.push(started.elapsed());
        let payload: PlaybackCommandPayload = serde_json::from_value(event.payload).unwrap();
        assert_eq!(incoming_of(&payload), Some(expected));
        assert_eq!(payload.from_device_name.as_deref(), Some("Новый телефон"));
    }
    println!("задержка команд: {latency:?}");
    assert!(latency.iter().all(|l| *l < Duration::from_millis(500)), "дольше 0,5 с: {latency:?}");

    // Нажатие по треку в списке: очередь и индекс.
    assert!(control.play_queue(&tracks(), 2));
    let event = next_event(&mut computer_events, "playback.command").await;
    let payload: PlaybackCommandPayload = serde_json::from_value(event.payload).unwrap();
    match incoming_of(&payload) {
        Some(Incoming::PlayQueue { tracks, index, .. }) => {
            assert_eq!((tracks.len(), index, tracks[index].title.as_str()), (3, 2, "Photosynthesis"))
        }
        other => panic!("{other:?}"),
    }

    // «Слушать здесь»: телефон забирает очередь и место, компьютер уступает.
    let state = control.state_to_take().await.expect("есть что забрать");
    assert_eq!(state.queue.len(), 3);
    let phone_reporter = Reporter::new(
        Port {
            shot: Mutex::new(PlayerSnapshot {
                tracks: tracks(),
                index: 0,
                position_ms: 83_000,
                duration_ms: Some(213_000),
                playing: true,
                volume: Some(50),
            }),
            account: Arc::clone(&pair.second),
            clock: Arc::clone(&phone_clock),
        },
        handle.clone(),
        |_| {},
    );
    phone_reporter.take_over(PlaybackHandoffInput { device_id: state.device_id.clone(), session_id: state.session_id.clone() });
    let event = next_event(&mut computer_events, "playback.updated").await;
    let payload: PlaybackUpdatedPayload = serde_json::from_value(event.payload).unwrap();
    let state = payload.state.expect("состояние телефона");
    assert!(should_give_way(state.handoff_from.as_ref(), Some(&first_id), &reporter.session_id(), clock.now()), "компьютер ставит паузу");
    control.disconnect();

    // Управление выключено: компьютер переоткрыл поток без remote=1.
    computer_stream.abort();
    let (computer_stream, mut computer_events) = open_events(&pair.first, false);
    next_event(&mut computer_events, "system.connected").await;
    let notices = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&notices);
    control.subscribe_notices(move |n| sink.lock().unwrap().push(n.clone()));
    let computer = control.devices().await.unwrap().into_iter().find(|d| d.device_id == first_id).unwrap();
    assert!(computer.online && !computer.controllable, "в сети, но управление выключено");
    control.connect(&computer);
    control.pause();
    wait_for(|| (!notices.lock().unwrap().is_empty()).then_some(())).await;
    assert_eq!(notices.lock().unwrap()[0], RemoteNotice::Disabled("Компьютер".into()));
    assert!(!control.active());

    // Устройство не в сети.
    computer_stream.abort();
    tokio::time::sleep(Duration::from_millis(800)).await;
    control.connect(&computer);
    control.pause();
    wait_for(|| (notices.lock().unwrap().len() >= 2).then_some(())).await;
    assert_eq!(notices.lock().unwrap()[1], RemoteNotice::Offline("Компьютер".into()));

    reporter.stop();
    phone_reporter.stop();
    phone_stream.abort();
    pair.cleanup().await;
}

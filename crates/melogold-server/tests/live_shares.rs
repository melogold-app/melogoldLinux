//! Ссылки на свои плейлисты против настоящего сервера (задание 0010):
//!
//! ```sh
//! MELOGOLD_LIVE_SERVER=http://127.0.0.1:18080 cargo test -p melogold-server --test live_shares -- --ignored --test-threads=1
//! ```

mod common;

use common::{pair, server};
use melogold_core::music::Track;
use melogold_core::share_links::parse_share_url;
use melogold_server::dto::TrackInput;

fn tracks() -> Vec<Track> {
    [("dQw4w9WgXcQ", "Never Gonna Give You Up", "Rick Astley"), ("fJ9rUzIMcZQ", "Bohemian Rhapsody", "Queen")]
        .into_iter()
        .map(|(id, title, artist)| Track {
            video_id: id.into(),
            title: title.into(),
            artists_text: Some(artist.into()),
            duration_ms: Some(200_000),
            ..Default::default()
        })
        .collect()
}

#[tokio::test]
#[ignore = "нужен локальный сервер: MELOGOLD_LIVE_SERVER"]
async fn share_create_open_without_login_list_delete() {
    let pair = pair().await;
    assert!(pair.first.ensure_server_info().await.unwrap().features.share.is_some(), "сервер без features.share");
    let inputs: Vec<TrackInput> = tracks().iter().map(TrackInput::from_track).collect();
    let created = pair.first.create_share("Вечер", inputs).await.expect("снимок создаётся");
    assert!(created.url.ends_with(&format!("/s/{}", created.share_id)));
    // Ссылка, которую отдаёт сервер, разбирается так же, как вставленная в поиск.
    let parsed = parse_share_url(&created.url).expect("ссылка /s/<код> разбирается");
    assert_eq!(parsed.id, created.share_id);

    // Открыть на другом устройстве без входа: второй аккаунт не входил.
    let shared = pair.second.open_share(&parsed.base, &parsed.id).await.expect("снимок открывается без входа");
    assert_eq!(shared.name, "Вечер");
    let titles: Vec<String> = shared.tracks.iter().map(|t| t.to_track().title).collect();
    assert_eq!(titles, ["Never Gonna Give You Up", "Bohemian Rhapsody"], "порядок треков сохранён");

    let list = pair.first.shares().await.unwrap();
    assert!(list.shares.iter().any(|s| s.share_id == created.share_id));

    pair.first.delete_share(&created.share_id).await.expect("ссылка удаляется");
    let gone = pair.second.open_share(&server(), &created.share_id).await.expect_err("удалённая ссылка не открывается");
    assert_eq!((gone.status, gone.code.as_str()), (404, "share_not_found"));
    let again = pair.first.delete_share(&created.share_id).await.expect_err("второй раз — 404");
    assert_eq!(again.status, 404);
    pair.cleanup().await;
}

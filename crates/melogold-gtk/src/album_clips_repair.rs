//! Починка медиатеки (задание 0030): клипы из альбомов, сохранённые до задания 0029, — на песни.
//!
//! До 0029 список альбома давал клипы (`MUSIC_VIDEO_TYPE_OMV`) вместо песен, и то, что человек
//! лайкнул или положил в плейлист из альбома, осталось клипом: LEMONHEAD у Tyler, The Creator
//! играл клип на 1:25 вместо песни на 2:11. Один раз после запуска для каждого альбома с такими
//! клипами загружается альбом с песнями, и ♡ и места в плейлистах переходят на песни. Синхронизация
//! разнесёт это по остальным устройствам.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use melogold_data::library::Library;
use melogold_innertube::music::YouTubeMusic;

/// Отметка в `app_state`: починка прошла целиком.
const DONE: &str = "repair.album_clips";
/// Не мешать запуску: окно, очередь и первая синхронизация — раньше.
const START_DELAY: std::time::Duration = std::time::Duration::from_secs(20);

pub async fn run(library: Arc<Library>, music: YouTubeMusic) {
    let done = {
        let library = Arc::clone(&library);
        tokio::task::spawn_blocking(move || library.state(DONE)).await
    };
    if matches!(done, Ok(Ok(Some(_)))) {
        return;
    }
    tokio::time::sleep(START_DELAY).await;

    let clips = {
        let library = Arc::clone(&library);
        tokio::task::spawn_blocking(move || library.saved_album_clips()).await
    };
    let clips = match clips {
        Ok(Ok(clips)) => clips,
        _ => return,
    };
    let mut albums: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for clip in clips {
        if let Some(album) = clip.album_id {
            albums.entry(album).or_default().insert(clip.video_id);
        }
    }

    let (mut replaced, mut failed) = (0usize, false);
    for (album, clips) in &albums {
        let twins = match music.album_twins(album).await {
            Ok(twins) => twins,
            Err(error) => {
                // Без сети — в следующий запуск: отметка о готовности не ставится.
                tracing::warn!(альбом = %album, %error, "починка клипов: альбом не загрузился");
                failed = true;
                continue;
            }
        };
        for (clip, song) in twins.into_iter().filter(|(clip, _)| clips.contains(clip)) {
            let library = Arc::clone(&library);
            let result =
                tokio::task::spawn_blocking(move || library.replace_clip_with_song(&clip, &song).map(|r| (clip, song.video_id, r))).await;
            match result {
                Ok(Ok((clip, song, what))) => {
                    replaced += 1;
                    tracing::info!(альбом = %album, клип = %clip, песня = %song, ?what, "клип в медиатеке заменён песней");
                }
                _ => failed = true,
            }
        }
    }

    tracing::info!(альбомов = albums.len(), заменено = replaced, повторить = failed, "починка клипов из альбомов");
    if !failed {
        let library = Arc::clone(&library);
        let _ = tokio::task::spawn_blocking(move || library.set_state(DONE, Some("1"))).await;
    }
}

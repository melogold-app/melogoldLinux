//! Очередь по REWRITE §4.10.4 Android, одинаковая на всех клиентах (Windows `PlayQueue.cs`).
//!
//! Состав: пользовательские элементы, за ними блок автовоспроизведения. «Играть следующим» —
//! сразу после текущего; «В конец очереди» — перед первым элементом автовоспроизведения;
//! перетаскивание элемента автовоспроизведения в пользовательскую часть делает его
//! пользовательским. Перемешивание трогает только пользовательскую часть, текущий трек
//! первым, блок автовоспроизведения остаётся в конце; выключение возвращает исходный порядок.
//! Повтор «всё» убирает блок автовоспроизведения. Чистая модель без плеера.

use serde::{Deserialize, Serialize};

use crate::music::Track;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

impl RepeatMode {
    /// Кнопка повтора: выкл → всё → один → выкл.
    pub fn next(self) -> RepeatMode {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub track: Track,
    pub from_autoplay: bool,
    pub id: i64,
}

/// Снимок для «Очередь заменена · Отменить» и восстановления после перезапуска.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QueueSnapshot {
    pub items: Vec<QueueItem>,
    pub index: i64,
    pub shuffle_order: Option<Vec<usize>>,
    pub position_ms: i64,
    pub repeat: RepeatMode,
}

#[derive(Clone, Debug, Default)]
pub struct PlayQueue {
    items: Vec<QueueItem>,
    shuffle: Option<Vec<usize>>,
    current: Option<usize>,
    repeat: RepeatMode,
    next_id: i64,
}

impl PlayQueue {
    pub fn new() -> Self {
        Self { next_id: 1, ..Default::default() }
    }

    /// Элементы в исходном порядке (без перемешивания).
    pub fn items(&self) -> &[QueueItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    pub fn current(&self) -> Option<&QueueItem> {
        self.current.and_then(|i| self.items.get(i))
    }

    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    pub fn is_shuffled(&self) -> bool {
        self.shuffle.is_some()
    }

    /// Порядок проигрывания: индексы `items`.
    pub fn play_order(&self) -> Vec<usize> {
        self.shuffle.clone().unwrap_or_else(|| (0..self.items.len()).collect())
    }

    /// Элементы в порядке проигрывания (панель очереди).
    pub fn ordered(&self) -> Vec<&QueueItem> {
        self.play_order().into_iter().map(|i| &self.items[i]).collect()
    }

    pub fn autoplay_start(&self) -> usize {
        self.items.iter().position(|i| i.from_autoplay).unwrap_or(self.items.len())
    }

    /// Сколько элементов добавил пользователь: «Очередь заменена · Отменить» — от двух.
    pub fn user_added_count(&self) -> usize {
        self.items.iter().filter(|i| !i.from_autoplay).count()
    }

    fn item(&mut self, track: Track, autoplay: bool) -> QueueItem {
        let id = self.next_id;
        self.next_id += 1;
        QueueItem { track, from_autoplay: autoplay, id }
    }

    fn position_in_order(&self, order: &[usize]) -> Option<usize> {
        self.current.and_then(|c| order.iter().position(|&i| i == c))
    }

    // ── замена ──

    /// Играть список с выбранного трека. При перемешивании выбранный — первым.
    pub fn set_list(&mut self, tracks: Vec<Track>, start: usize, shuffle: bool) {
        self.items.clear();
        for track in tracks {
            let item = self.item(track, false);
            self.items.push(item);
        }
        self.current = (!self.items.is_empty()).then(|| start.min(self.items.len() - 1));
        self.shuffle = None;
        if shuffle {
            self.set_shuffle(true, &mut rand_order);
        }
    }

    /// Одиночный трек (поиск, «Недавние», ссылка): дальше — автовоспроизведение похожих.
    pub fn set_single(&mut self, track: Track) {
        self.set_list(vec![track], 0, false);
    }

    pub fn snapshot(&self, position_ms: i64) -> QueueSnapshot {
        QueueSnapshot {
            items: self.items.clone(),
            index: self.current.map(|i| i as i64).unwrap_or(-1),
            shuffle_order: self.shuffle.clone(),
            position_ms,
            repeat: self.repeat,
        }
    }

    pub fn restore(&mut self, snapshot: QueueSnapshot) {
        self.items = snapshot.items;
        self.next_id = self.items.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        self.current = if self.items.is_empty() { None } else { Some((snapshot.index.max(0) as usize).min(self.items.len() - 1)) };
        self.shuffle = snapshot.shuffle_order.filter(|order| {
            let mut sorted = order.clone();
            sorted.sort_unstable();
            sorted == (0..self.items.len()).collect::<Vec<_>>()
        });
        self.repeat = snapshot.repeat;
    }

    // ── вставка ──

    /// «Играть следующим»: сразу после текущего (в порядке проигрывания).
    pub fn play_next(&mut self, tracks: Vec<Track>) {
        if tracks.is_empty() {
            return;
        }
        let Some(current) = self.current else {
            self.set_list(tracks, 0, false);
            return;
        };
        let count = tracks.len();
        let new_items: Vec<QueueItem> = tracks.into_iter().map(|t| self.item(t, false)).collect();
        // В исходном порядке — сразу после текущего и в пользовательской части.
        let mut insert_at = (current + 1).min(self.autoplay_start());
        if insert_at <= current {
            insert_at = current + 1;
        }
        self.items.splice(insert_at..insert_at, new_items);
        if let Some(order) = self.shuffle.take() {
            let mut shifted: Vec<usize> = order.into_iter().map(|i| if i >= insert_at { i + count } else { i }).collect();
            let position = shifted.iter().position(|&i| i == current).map(|p| p + 1).unwrap_or(shifted.len());
            shifted.splice(position..position, insert_at..insert_at + count);
            self.shuffle = Some(shifted);
        }
    }

    /// «В конец очереди»: перед блоком автовоспроизведения.
    pub fn add_to_end(&mut self, tracks: Vec<Track>) {
        if tracks.is_empty() {
            return;
        }
        let Some(current) = self.current else {
            self.set_list(tracks, 0, false);
            return;
        };
        let insert_at = self.autoplay_start();
        let count = tracks.len();
        let new_items: Vec<QueueItem> = tracks.into_iter().map(|t| self.item(t, false)).collect();
        self.items.splice(insert_at..insert_at, new_items);
        if insert_at <= current {
            self.current = Some(current + count);
        }
        if let Some(order) = self.shuffle.take() {
            let mut shifted: Vec<usize> = order.into_iter().map(|i| if i >= insert_at { i + count } else { i }).collect();
            // В перемешанном порядке — перед первым элементом автовоспроизведения.
            let first_auto = shifted.iter().position(|&i| i >= insert_at + count).unwrap_or(shifted.len());
            shifted.splice(first_auto..first_auto, insert_at..insert_at + count);
            self.shuffle = Some(shifted);
        }
    }

    /// Догрузка автовоспроизведения в конец.
    pub fn append_autoplay(&mut self, tracks: Vec<Track>) {
        if tracks.is_empty() || self.repeat == RepeatMode::All {
            return;
        }
        let start = self.items.len();
        let count = tracks.len();
        for track in tracks {
            let item = self.item(track, true);
            self.items.push(item);
        }
        if let Some(order) = self.shuffle.as_mut() {
            order.extend(start..start + count);
        }
        if self.current.is_none() {
            self.current = Some(0);
        }
    }

    /// Сколько элементов автовоспроизведения осталось впереди.
    pub fn autoplay_ahead(&self) -> usize {
        let order = self.play_order();
        let position = self.position_in_order(&order).map(|p| p + 1).unwrap_or(0);
        order[position.min(order.len())..].iter().filter(|&&i| self.items[i].from_autoplay).count()
    }

    // ── удаление и перестановка ──

    pub fn remove(&mut self, id: i64) {
        let Some(index) = self.items.iter().position(|i| i.id == id) else { return };
        if Some(index) == self.current {
            return;
        }
        self.items.remove(index);
        if let Some(current) = self.current {
            if index < current {
                self.current = Some(current - 1);
            }
        }
        if let Some(order) = self.shuffle.take() {
            self.shuffle = Some(order.into_iter().filter(|&i| i != index).map(|i| if i > index { i - 1 } else { i }).collect());
        }
    }

    /// «Очистить»: всё, кроме текущего трека.
    pub fn clear_except_current(&mut self) {
        let Some(current) = self.current().cloned() else { return };
        self.items = vec![current];
        self.current = Some(0);
        if self.shuffle.is_some() {
            self.shuffle = Some(vec![0]);
        }
    }

    /// Перенести элемент на место `to` в порядке проигрывания. Элемент автовоспроизведения,
    /// перенесённый в пользовательскую часть, становится пользовательским.
    pub fn move_item(&mut self, id: i64, to: usize) {
        let Some(index) = self.items.iter().position(|i| i.id == id) else { return };
        if let Some(order) = self.shuffle.as_mut() {
            if let Some(from) = order.iter().position(|&i| i == index) {
                order.remove(from);
                let target = to.min(order.len());
                order.insert(target, index);
            }
            return;
        }
        let current_id = self.current().map(|i| i.id);
        let mut item = self.items.remove(index);
        let target = to.min(self.items.len());
        if item.from_autoplay && target <= self.autoplay_start() {
            item.from_autoplay = false;
        }
        self.items.insert(target, item);
        if let Some(cid) = current_id {
            self.current = self.items.iter().position(|i| i.id == cid);
        }
    }

    // ── переходы ──

    pub fn jump_to(&mut self, id: i64) -> bool {
        match self.items.iter().position(|i| i.id == id) {
            Some(index) => {
                self.current = Some(index);
                true
            }
            None => false,
        }
    }

    /// Индекс следующего элемента в порядке проигрывания или `None` (конец очереди).
    pub fn peek_next(&self, user_action: bool) -> Option<usize> {
        let current = self.current?;
        if self.repeat == RepeatMode::One && !user_action {
            return Some(current);
        }
        let order = self.play_order();
        let position = self.position_in_order(&order)?;
        if position + 1 < order.len() {
            Some(order[position + 1])
        } else if self.repeat == RepeatMode::All && !order.is_empty() {
            Some(order[0])
        } else {
            None
        }
    }

    pub fn move_next(&mut self, user_action: bool) -> bool {
        match self.peek_next(user_action) {
            Some(next) => {
                self.current = Some(next);
                true
            }
            None => false,
        }
    }

    pub fn move_previous(&mut self) -> bool {
        let order = self.play_order();
        let Some(position) = self.position_in_order(&order) else { return false };
        if position > 0 {
            self.current = Some(order[position - 1]);
        } else if self.repeat == RepeatMode::All && order.len() > 1 {
            self.current = order.last().copied();
        } else {
            return false;
        }
        true
    }

    /// Индексы следующих `count` элементов (упреждающий резолв потока).
    pub fn upcoming(&self, count: usize) -> Vec<usize> {
        let order = self.play_order();
        let position = self.position_in_order(&order).map(|p| p + 1).unwrap_or(0);
        order.into_iter().skip(position).take(count).collect()
    }

    // ── режимы ──

    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
        if mode != RepeatMode::All {
            return;
        }
        // При повторе очереди автовоспроизведение не добавляется, имеющийся блок убирается.
        let current_id = self.current().map(|i| i.id);
        if let Some(current) = self.current {
            self.items[current].from_autoplay = false;
        }
        let removed: Vec<usize> = self.items.iter().enumerate().filter(|(_, i)| i.from_autoplay).map(|(n, _)| n).collect();
        if removed.is_empty() {
            return;
        }
        self.items.retain(|i| !i.from_autoplay);
        if let Some(order) = self.shuffle.take() {
            self.shuffle =
                Some(order.into_iter().filter(|i| !removed.contains(i)).map(|i| i - removed.iter().filter(|&&r| r < i).count()).collect());
        }
        if let Some(cid) = current_id {
            self.current = self.items.iter().position(|i| i.id == cid);
        }
    }

    /// Перемешать; `shuffled` получает пользовательскую часть без текущего и возвращает её в новом порядке.
    pub fn set_shuffle(&mut self, on: bool, shuffled: &mut dyn FnMut(Vec<usize>) -> Vec<usize>) {
        if !on {
            self.shuffle = None;
            return;
        }
        let autoplay_start = self.autoplay_start();
        let user: Vec<usize> = (0..autoplay_start).filter(|&i| Some(i) != self.current).collect();
        let mut order = Vec::with_capacity(self.items.len());
        if let Some(current) = self.current.filter(|&c| c < autoplay_start) {
            order.push(current);
        }
        order.extend(shuffled(user));
        // Автовоспроизведение — в конце по порядку; если играет трек из него, он остаётся текущим.
        order.extend(autoplay_start..self.items.len());
        if let Some(current) = self.current.filter(|&c| c >= autoplay_start) {
            order.retain(|&i| i != current);
            order.insert(0, current);
        }
        self.shuffle = Some(order);
    }

    pub fn shuffle(&mut self, on: bool) {
        self.set_shuffle(on, &mut rand_order);
    }
}

/// Перемешивание Фишера — Йетса на простом генераторе от часов: криптостойкость здесь не нужна.
fn rand_order(mut items: Vec<usize>) -> Vec<usize> {
    let mut state = crate::text::now_ms() as u64 ^ 0x9E37_79B9_7F4A_7C15 ^ (items.len() as u64).rotate_left(17);
    for i in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let j = (state % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str) -> Track {
        Track { video_id: format!("id-{title}"), title: title.into(), ..Default::default() }
    }

    fn tracks(titles: &[&str]) -> Vec<Track> {
        titles.iter().map(|t| track(t)).collect()
    }

    fn titles(queue: &PlayQueue) -> Vec<String> {
        queue.items().iter().map(|i| i.track.title.clone()).collect()
    }

    fn ordered(queue: &PlayQueue) -> Vec<String> {
        queue.ordered().iter().map(|i| i.track.title.clone()).collect()
    }

    fn current(queue: &PlayQueue) -> &str {
        &queue.current().unwrap().track.title
    }

    #[test]
    fn list_starts_at_chosen_track() {
        let mut queue = PlayQueue::new();
        queue.set_list(tracks(&["a", "b", "c"]), 1, false);
        assert_eq!(current(&queue), "b");
        assert!(queue.move_next(false));
        assert_eq!(current(&queue), "c");
        assert!(!queue.move_next(false));
    }

    #[test]
    fn play_next_goes_right_after_current_and_add_to_end_before_autoplay() {
        let mut queue = PlayQueue::new();
        queue.set_single(track("a"));
        queue.append_autoplay(tracks(&["x", "y"]));
        queue.play_next(tracks(&["next"]));
        queue.add_to_end(tracks(&["end"]));
        assert_eq!(titles(&queue), ["a", "next", "end", "x", "y"]);
        assert!(!queue.items()[2].from_autoplay);
        assert!(queue.items()[3].from_autoplay);
    }

    #[test]
    fn shuffle_keeps_current_first_and_autoplay_last() {
        let mut queue = PlayQueue::new();
        queue.set_list(tracks(&["a", "b", "c", "d", "e"]), 2, false);
        queue.append_autoplay(tracks(&["x", "y"]));
        queue.shuffle(true);
        let order = ordered(&queue);
        assert_eq!(order[0], "c");
        assert_eq!(order[5..], ["x", "y"]);
        let mut middle = order[1..5].to_vec();
        middle.sort();
        assert_eq!(middle, ["a", "b", "d", "e"]);
        queue.shuffle(false);
        assert_eq!(titles(&queue), ["a", "b", "c", "d", "e", "x", "y"]);
        assert_eq!(current(&queue), "c");
    }

    #[test]
    fn repeat_all_drops_autoplay_and_wraps() {
        let mut queue = PlayQueue::new();
        queue.set_list(tracks(&["a", "b"]), 1, false);
        queue.append_autoplay(tracks(&["x"]));
        queue.set_repeat(RepeatMode::All);
        assert_eq!(titles(&queue), ["a", "b"]);
        assert!(queue.move_next(false));
        assert_eq!(current(&queue), "a");
        queue.append_autoplay(tracks(&["z"]));
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn repeat_one_repeats_only_automatically() {
        let mut queue = PlayQueue::new();
        queue.set_list(tracks(&["a", "b"]), 0, false);
        queue.set_repeat(RepeatMode::One);
        assert_eq!(queue.peek_next(false), Some(0));
        assert_eq!(queue.peek_next(true), Some(1));
    }

    #[test]
    fn dragging_autoplay_item_up_makes_it_users() {
        let mut queue = PlayQueue::new();
        queue.set_single(track("a"));
        queue.append_autoplay(tracks(&["x", "y"]));
        let y = queue.items()[2].id;
        queue.move_item(y, 1);
        assert_eq!(titles(&queue), ["a", "y", "x"]);
        assert!(!queue.items()[1].from_autoplay);
        assert_eq!(queue.autoplay_start(), 2);
    }

    #[test]
    fn remove_keeps_current_and_snapshot_restores() {
        let mut queue = PlayQueue::new();
        queue.set_list(tracks(&["a", "b", "c"]), 1, false);
        let snapshot = queue.snapshot(1234);
        let a = queue.items()[0].id;
        let b = queue.items()[1].id;
        queue.remove(b);
        queue.remove(a);
        assert_eq!(current(&queue), "b");
        assert_eq!(queue.len(), 2);
        queue.restore(snapshot);
        assert_eq!(titles(&queue), ["a", "b", "c"]);
        assert_eq!(current(&queue), "b");
        queue.play_next(tracks(&["n"]));
        assert!(queue.items().iter().map(|i| i.id).collect::<std::collections::HashSet<_>>().len() == 4, "id не повторяются");
    }

    #[test]
    fn play_next_in_shuffle_goes_right_after_current() {
        let mut queue = PlayQueue::new();
        queue.set_list(tracks(&["a", "b", "c"]), 0, false);
        queue.set_shuffle(true, &mut |mut v| {
            v.reverse();
            v
        });
        assert_eq!(ordered(&queue), ["a", "c", "b"]);
        queue.play_next(tracks(&["n"]));
        assert_eq!(ordered(&queue), ["a", "n", "c", "b"]);
        queue.add_to_end(tracks(&["e"]));
        assert_eq!(ordered(&queue), ["a", "n", "c", "b", "e"]);
    }

    #[test]
    fn previous_and_upcoming() {
        let mut queue = PlayQueue::new();
        queue.set_list(tracks(&["a", "b", "c", "d"]), 1, false);
        assert_eq!(queue.upcoming(2), [2, 3]);
        assert!(queue.move_previous());
        assert_eq!(current(&queue), "a");
        assert!(!queue.move_previous());
        queue.set_repeat(RepeatMode::All);
        assert!(queue.move_previous());
        assert_eq!(current(&queue), "d");
    }
}

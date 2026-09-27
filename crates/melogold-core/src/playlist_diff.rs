//! Ops, которые превращают плейлист, каким его знал сервер (`before`, в его порядке), в плейлист
//! этого устройства (`after`) — синк со снимком (REWRITE §4.12a Android `PlaylistDiff.kt`, Windows
//! `PlaylistDiff.cs`): сначала убранные треки, затем в новом порядке новые треки блоками и
//! перемещённые, каждый — сразу после соседа в новом порядке (у стоящих в начале — перед первым
//! оставшимся). Треки самой длинной цепочки, сохранившей порядок, остаются на местах, поэтому
//! перенос одного трека — одна op.

use std::collections::{HashMap, HashSet};

/// Не больше стольких треков в одном `playlist.items.add` (API §4.8).
pub const MAX_ADD: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemChange {
    Remove(String),
    /// Новые треки блоком: сразу после `after`, иначе перед `before`, иначе в конец.
    Add {
        video_ids: Vec<String>,
        after: Option<String>,
        before: Option<String>,
    },
    /// Трек сменил место — так же.
    Move {
        video_id: String,
        after: Option<String>,
        before: Option<String>,
    },
}

pub fn changes(before: &[String], after: &[String]) -> Vec<ItemChange> {
    let mut changes = Vec::new();
    let after_set: HashSet<&str> = after.iter().map(String::as_str).collect();
    let mut before_index: HashMap<&str, usize> = HashMap::new();
    for (index, id) in before.iter().enumerate() {
        before_index.entry(id.as_str()).or_insert(index);
    }
    let mut removed: Vec<(&str, usize)> = before_index.iter().filter(|(id, _)| !after_set.contains(*id)).map(|(id, i)| (*id, *i)).collect();
    removed.sort_by_key(|(_, index)| *index);
    changes.extend(removed.into_iter().map(|(id, _)| ItemChange::Remove(id.to_owned())));

    let kept: Vec<&str> = after.iter().map(String::as_str).filter(|id| before_index.contains_key(id)).collect();
    let staying: HashSet<&str> =
        longest_increasing_run(&kept.iter().map(|id| before_index[id] as i64).collect::<Vec<_>>()).into_iter().map(|i| kept[i]).collect();
    let first_staying = after.iter().find(|id| staying.contains(id.as_str())).cloned();

    let mut previous: Option<String> = None;
    let mut block: Vec<String> = Vec::new();
    let mut block_after: Option<String> = None;
    let flush = |changes: &mut Vec<ItemChange>, block: &mut Vec<String>, block_after: &Option<String>| {
        let mut anchor = block_after.clone();
        for chunk in block.chunks(MAX_ADD) {
            let before = if anchor.is_none() { first_staying.clone() } else { None };
            changes.push(ItemChange::Add { video_ids: chunk.to_vec(), after: anchor.clone(), before });
            anchor = chunk.last().cloned();
        }
        block.clear();
    };
    for id in after {
        if !before_index.contains_key(id.as_str()) {
            if block.is_empty() {
                block_after = previous.clone();
            }
            block.push(id.clone());
        } else {
            flush(&mut changes, &mut block, &block_after);
            if !staying.contains(id.as_str()) {
                let before = if previous.is_none() { first_staying.clone() } else { None };
                changes.push(ItemChange::Move { video_id: id.clone(), after: previous.clone(), before });
            }
        }
        previous = Some(id.clone());
    }
    flush(&mut changes, &mut block, &block_after);
    changes
}

/// Индексы одной самой длинной строго возрастающей подпоследовательности (терпеливая сортировка).
pub fn longest_increasing_run(values: &[i64]) -> Vec<usize> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut tails: Vec<usize> = Vec::with_capacity(values.len());
    let mut parent = vec![usize::MAX; values.len()];
    for (index, value) in values.iter().enumerate() {
        let position = tails.partition_point(|&t| values[t] < *value);
        if position > 0 {
            parent[index] = tails[position - 1];
        }
        if position == tails.len() {
            tails.push(index);
        } else {
            tails[position] = index;
        }
    }
    let mut run = vec![0; tails.len()];
    let mut current = *tails.last().expect("не пусто");
    for slot in run.iter_mut().rev() {
        *slot = current;
        current = parent[current];
    }
    run
}

/// Применить изменения по правилам якорей (DESIGN §3.7): для проверки `apply(before, changes(before, after)) == after`.
pub fn apply(list: &[String], changes: &[ItemChange]) -> Vec<String> {
    let mut result = list.to_vec();
    let position = |list: &Vec<String>, after: &Option<String>, before: &Option<String>| -> usize {
        if let Some(index) = after.as_ref().and_then(|a| list.iter().position(|v| v == a)) {
            return index + 1;
        }
        if let Some(index) = before.as_ref().and_then(|b| list.iter().position(|v| v == b)) {
            return index;
        }
        list.len()
    };
    for change in changes {
        match change {
            ItemChange::Remove(id) => result.retain(|v| v != id),
            ItemChange::Add { video_ids, after, before } => {
                let mut fresh: Vec<String> = Vec::new();
                for id in video_ids {
                    if !result.contains(id) && !fresh.contains(id) {
                        fresh.push(id.clone());
                    }
                }
                let at = position(&result, after, before);
                result.splice(at..at, fresh);
            }
            ItemChange::Move { video_id, after, before } => {
                let Some(index) = result.iter().position(|v| v == video_id) else { continue };
                result.remove(index);
                let at = position(&result, after, before);
                result.insert(at, video_id.clone());
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn lis_vectors() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/playlist-ops.vectors.json")).unwrap();
        let spec: serde_json::Value = serde_json::from_str(&text).unwrap();
        for case in spec["lis"].as_array().unwrap() {
            let values: Vec<i64> = case["values"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
            let expected: Vec<usize> = case["indices"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
            assert_eq!(longest_increasing_run(&values).len(), expected.len(), "{case}");
            // Любая самая длинная цепочка годится; эта должна строго возрастать.
            let run = longest_increasing_run(&values);
            assert!(run.windows(2).all(|w| w[0] < w[1] && values[w[0]] < values[w[1]]), "{case}");
        }
    }

    #[test]
    fn anchor_vectors() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/playlist-ops.vectors.json")).unwrap();
        let spec: serde_json::Value = serde_json::from_str(&text).unwrap();
        let strings =
            |v: &serde_json::Value| -> Vec<String> { v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_owned()).collect() };
        let anchor = |case: &serde_json::Value, key: &str| case["anchors"][key].as_str().map(str::to_owned);
        let anchors = &spec["anchors"];
        for case in anchors["applyAdd"].as_array().unwrap() {
            let change = ItemChange::Add { video_ids: strings(&case["ids"]), after: anchor(case, "after"), before: anchor(case, "before") };
            assert_eq!(apply(&strings(&case["list"]), &[change]), strings(&case["result"]), "{case}");
        }
        for case in anchors["applyRemove"].as_array().unwrap() {
            let change = ItemChange::Remove(case["videoId"].as_str().unwrap().to_owned());
            assert_eq!(apply(&strings(&case["list"]), &[change]), strings(&case["result"]), "{case}");
        }
        for case in anchors["applyMove"].as_array().unwrap() {
            let change = ItemChange::Move {
                video_id: case["videoId"].as_str().unwrap().to_owned(),
                after: anchor(case, "after"),
                before: anchor(case, "before"),
            };
            assert_eq!(apply(&strings(&case["list"]), &[change]), strings(&case["result"]), "{case}");
        }
    }

    #[test]
    fn diff_rebuilds_the_new_order() {
        let cases = [
            ("a b c d", "a b c d"),
            ("a b c d", "a c d"),
            ("a b c d", "d a b c"),
            ("a b c d", "a b c d e f"),
            ("a b c d", "x a b y c d z"),
            ("a b c d", "b a d c"),
            ("", "a b"),
            ("a b", ""),
            ("a b c d e", "e d c b a"),
            ("a b c", "c x b y a"),
        ];
        for (before, after) in cases {
            let (before, after) = (ids(before), ids(after));
            let ops = changes(&before, &after);
            assert_eq!(apply(&before, &ops), after, "{before:?} → {after:?}: {ops:?}");
        }
        // Перенос одного трека — одна op.
        assert_eq!(changes(&ids("a b c d"), &ids("d a b c")).len(), 1);
        assert_eq!(changes(&ids("a b c d"), &ids("a b c d")), Vec::<ItemChange>::new());
    }

    #[test]
    fn long_additions_are_split() {
        let after: Vec<String> = (0..1200).map(|i| format!("v{i:04}")).collect();
        let ops = changes(&[], &after);
        assert_eq!(ops.len(), 3);
        assert_eq!(apply(&[], &ops), after);
    }
}

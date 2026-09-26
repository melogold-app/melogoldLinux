//! Навигация по ответам InnerTube: пути, `runs`, обложки. Всё терпит отсутствующие ключи
//! (Windows `JsonPath.cs`).

use serde_json::Value;

/// Шаг пути: ключ объекта или индекс массива.
pub trait Step {
    fn apply<'a>(&self, value: &'a Value) -> Option<&'a Value>;
}

impl Step for &str {
    fn apply<'a>(&self, value: &'a Value) -> Option<&'a Value> {
        value.get(*self)
    }
}

impl Step for i32 {
    fn apply<'a>(&self, value: &'a Value) -> Option<&'a Value> {
        usize::try_from(*self).ok().and_then(|index| value.get(index))
    }
}

impl Step for usize {
    fn apply<'a>(&self, value: &'a Value) -> Option<&'a Value> {
        value.get(*self)
    }
}

/// Начало пути: и `&Value`, и `Option<&Value>`.
pub trait Node<'a> {
    fn node(self) -> Option<&'a Value>;
}

impl<'a> Node<'a> for &'a Value {
    fn node(self) -> Option<&'a Value> {
        Some(self)
    }
}

impl<'a> Node<'a> for Option<&'a Value> {
    fn node(self) -> Option<&'a Value> {
        self
    }
}

/// `at!(node, "contents", 0, "tabRenderer")` → `Option<&Value>`.
#[macro_export]
macro_rules! at {
    ($node:expr $(, $step:expr)* $(,)?) => {{
        let current = $crate::json::Node::node($node);
        $( let current = current.and_then(|value| $crate::json::Step::apply(&$step, value)); )*
        current
    }};
}

/// Кусок подписи с возможным переходом.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub text: String,
    pub browse_id: Option<String>,
    pub page_type: Option<String>,
    pub watch_video_id: Option<String>,
    pub watch: Option<Value>,
}

impl Run {
    pub fn separator() -> Run {
        Run { text: " • ".into(), ..Default::default() }
    }

    fn from(node: &Value) -> Run {
        let browse = at!(node, "navigationEndpoint", "browseEndpoint");
        Run {
            text: at!(node, "text").str().unwrap_or_default().to_owned(),
            browse_id: at!(browse, "browseId").string(),
            page_type: at!(browse, "browseEndpointContextSupportedConfigs", "browseEndpointContextMusicConfig", "pageType").string(),
            watch_video_id: at!(node, "navigationEndpoint", "watchEndpoint", "videoId").string(),
            watch: at!(node, "navigationEndpoint", "watchEndpoint").cloned(),
        }
    }

    pub fn is_separator(&self) -> bool {
        matches!(self.text.as_str(), " • " | " · " | "•")
    }
}

pub trait Json<'a> {
    fn str(self) -> Option<&'a str>;
    fn string(self) -> Option<String>;
    /// Число или строка с числом.
    fn i64(self) -> Option<i64>;
    fn f64(self) -> Option<f64>;
    fn flag(self) -> bool;
    fn items(self) -> &'a [Value];
    /// Текст узла `{runs: […]}`, `{simpleText}` или `{content}`.
    fn text(self) -> Option<String>;
    fn runs(self) -> Vec<Run>;
    /// Самая большая обложка из массива `thumbnails`.
    fn best_thumbnail(self) -> Option<String>;
    /// Первое вложенное значение с ключом (обход в глубину).
    fn find(self, key: &str) -> Option<&'a Value>;
    /// Все вложенные значения с ключом.
    fn find_all(self, key: &str) -> Vec<&'a Value>;
}

impl<'a> Json<'a> for Option<&'a Value> {
    fn str(self) -> Option<&'a str> {
        self.and_then(Value::as_str)
    }

    fn string(self) -> Option<String> {
        self.str().map(str::to_owned)
    }

    fn i64(self) -> Option<i64> {
        match self? {
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    fn f64(self) -> Option<f64> {
        match self? {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    fn flag(self) -> bool {
        self.and_then(Value::as_bool).unwrap_or(false)
    }

    fn items(self) -> &'a [Value] {
        self.and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
    }

    fn text(self) -> Option<String> {
        let node = self?;
        if let Some(runs) = node.get("runs").and_then(Value::as_array) {
            return Some(runs.iter().filter_map(|r| r.get("text").and_then(Value::as_str)).collect());
        }
        at!(node, "simpleText").string().or_else(|| at!(node, "content").string())
    }

    fn runs(self) -> Vec<Run> {
        at!(self, "runs").items().iter().map(Run::from).collect()
    }

    fn best_thumbnail(self) -> Option<String> {
        self.items().iter().max_by_key(|t| at!(*t, "width").i64().unwrap_or(0)).and_then(|t| at!(t, "url").string())
    }

    fn find(self, key: &str) -> Option<&'a Value> {
        match self? {
            Value::Object(map) => {
                if let Some(direct) = map.get(key).filter(|v| !v.is_null()) {
                    return Some(direct);
                }
                map.values().find_map(|v| Some(v).find(key))
            }
            Value::Array(items) => items.iter().find_map(|v| Some(v).find(key)),
            _ => None,
        }
    }

    fn find_all(self, key: &str) -> Vec<&'a Value> {
        let mut found = Vec::new();
        collect(self, key, &mut found);
        found
    }
}

fn collect<'a>(node: Option<&'a Value>, key: &str, found: &mut Vec<&'a Value>) {
    match node {
        Some(Value::Object(map)) => {
            for (name, value) in map {
                if value.is_null() {
                    continue;
                }
                if name == key {
                    found.push(value);
                } else {
                    collect(Some(value), key, found);
                }
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                collect(Some(item), key, found);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paths_texts_and_thumbnails() {
        let value = json!({
            "a": [{"b": {"runs": [{"text": "x"}, {"text": "y", "navigationEndpoint": {"browseEndpoint": {"browseId": "UC1"}}}]}}],
            "t": [{"url": "s", "width": 60}, {"url": "l", "width": 544}],
            "n": "42",
        });
        assert_eq!(at!(&value, "a", 0, "b").text().as_deref(), Some("xy"));
        assert_eq!(at!(&value, "a", 0, "b").runs()[1].browse_id.as_deref(), Some("UC1"));
        assert_eq!(at!(&value, "a", 5, "b"), None);
        assert_eq!(at!(&value, "t").best_thumbnail().as_deref(), Some("l"));
        assert_eq!(at!(&value, "n").i64(), Some(42));
        assert_eq!(Some(&value).find("browseId").str(), Some("UC1"));
        assert_eq!(Some(&value).find_all("text").len(), 2);
    }
}

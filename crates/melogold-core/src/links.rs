//! Ссылки `melogold://` (API §7.2).
//!
//! Разбор только раскладывает ссылку по полям. Проверку адреса сервера (§7.1) и всё, что
//! идёт к серверу, делает экран, который ссылку открыл. Автоматического входа или
//! одобрения по ссылке не бывает никогда.

use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MelogoldLink {
    /// `melogold://server?v=1&url=…&sid=…` — экран «Сервер» с заполненным адресом и подтверждением.
    Server { url: String, server_id: Option<String> },
    /// `melogold://link?mode=invite…` — «Войти с другого устройства» с явной кнопкой «Подключиться».
    Invite { server: String, server_id: String, token: String },
    /// `melogold://link?mode=request…` — не выполняется: только инструкция.
    Request { server: String, server_id: String },
    /// Схема наша, но разобрать нельзя: другая версия, нет полей, незнакомый хост.
    Unsupported,
}

pub fn is_melogold_link(text: &str) -> bool {
    text.trim().get(..11).is_some_and(|scheme| scheme.eq_ignore_ascii_case("melogold://"))
}

/// Разбирает `melogold://…`; для любой другой строки — `None`.
pub fn parse(text: &str) -> Option<MelogoldLink> {
    let text = text.trim();
    if !is_melogold_link(text) {
        return None;
    }
    let Ok(url) = Url::parse(text) else { return Some(MelogoldLink::Unsupported) };
    let query = |name: &str| url.query_pairs().find(|(key, _)| key == name).map(|(_, value)| value.into_owned());
    // Версия ссылок — 1; без `v` ссылку тоже принимаем: старые кнопки сервера её не писали.
    if query("v").is_some_and(|version| version != "1") {
        return Some(MelogoldLink::Unsupported);
    }
    let non_empty = |value: Option<String>| value.filter(|value| !value.is_empty());
    let link = match url.host_str().map(str::to_ascii_lowercase).as_deref() {
        Some("server") => match non_empty(query("url")) {
            Some(url) => MelogoldLink::Server { url, server_id: non_empty(query("sid")) },
            None => MelogoldLink::Unsupported,
        },
        Some("link") => match (query("mode").as_deref(), non_empty(query("server")), non_empty(query("sid"))) {
            (Some("invite"), Some(server), Some(server_id)) => match non_empty(query("token")) {
                Some(token) => MelogoldLink::Invite { server, server_id, token },
                None => MelogoldLink::Unsupported,
            },
            (Some("request"), Some(server), Some(server_id)) => MelogoldLink::Request { server, server_id },
            _ => MelogoldLink::Unsupported,
        },
        _ => MelogoldLink::Unsupported,
    };
    Some(link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_link_is_percent_decoded() {
        assert_eq!(
            parse("melogold://server?v=1&url=https%3A%2F%2Fmusic.example.com&sid=5b1c"),
            Some(MelogoldLink::Server { url: "https://music.example.com".into(), server_id: Some("5b1c".into()) })
        );
        assert_eq!(
            parse("  MELOGOLD://server?url=http%3A%2F%2F192.168.1.50%3A8080 "),
            Some(MelogoldLink::Server { url: "http://192.168.1.50:8080".into(), server_id: None })
        );
    }

    #[test]
    fn link_modes() {
        assert_eq!(
            parse("melogold://link?v=1&mode=invite&server=https%3A%2F%2Fa.b&sid=s&token=t"),
            Some(MelogoldLink::Invite { server: "https://a.b".into(), server_id: "s".into(), token: "t".into() })
        );
        assert_eq!(
            parse("melogold://link?mode=request&server=https%3A%2F%2Fa.b&sid=s&token=t&extra=1"),
            Some(MelogoldLink::Request { server: "https://a.b".into(), server_id: "s".into() })
        );
    }

    #[test]
    fn broken_links_are_unsupported_not_ignored() {
        assert_eq!(parse("melogold://server?v=2&url=x"), Some(MelogoldLink::Unsupported));
        assert_eq!(parse("melogold://link?mode=invite&server=a&sid=b"), Some(MelogoldLink::Unsupported));
        assert_eq!(parse("melogold://play?id=1"), Some(MelogoldLink::Unsupported));
        assert_eq!(parse("https://youtu.be/dQw4w9WgXcQ"), None);
    }
}

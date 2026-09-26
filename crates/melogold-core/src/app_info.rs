//! Имя, версия и адреса приложения. Версия задаётся одним местом — `Cargo.toml` workspace.

/// Идентификатор приложения: GApplication, `.desktop`, иконки, D-Bus (docs/PROMPT.md §3 «Идентичность»).
pub const APP_ID: &str = "app.melogold.Melogold";

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const REPOSITORY_URL: &str = "https://github.com/melogold-app/melogoldLinux";

pub const ISSUES_URL: &str = "https://github.com/melogold-app/melogoldLinux/issues";

/// `platform` в `DeviceInput` и в User-Agent сервера (API §1.2, §1.6).
pub const PLATFORM: &str = "linux";

/// Рабочий сервер по умолчанию — константа сборки (docs/PROMPT.md §1).
pub const DEFAULT_SERVER_URL: &str = "https://178-250-187-202.sslip.io";

/// User-Agent запросов к серверу Melogold: `melogold-linux/<semver>` (API §1.2).
pub fn server_user_agent() -> String {
    format!("melogold-{PLATFORM}/{VERSION}")
}

/// User-Agent LrcLib, KuGou и GitHub (docs/PROMPT.md §3 «Сеть»).
pub fn tool_user_agent() -> String {
    format!("Melogold/{VERSION} (+{REPOSITORY_URL})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agents_follow_the_contract() {
        assert_eq!(server_user_agent(), format!("melogold-linux/{VERSION}"));
        assert!(tool_user_agent().starts_with("Melogold/"));
        assert!(tool_user_agent().ends_with("(+https://github.com/melogold-app/melogoldLinux)"));
    }

    #[test]
    fn version_has_no_suffix() {
        assert!(VERSION.split('.').count() == 3 && VERSION.chars().all(|c| c.is_ascii_digit() || c == '.'), "{VERSION}");
    }
}

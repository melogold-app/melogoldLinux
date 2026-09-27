//! Где лежит сессия (docs/PROMPT.md §3 «Где что лежит»): токены — в связке ключей (Secret
//! Service, в песочнице — портал). Если её нет, то файл `0600` в данных приложения, и
//! «Диагностика» говорит, что токены лежат без связки ключей. Пароль не хранится нигде.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Сессия на сервере, как её хранит устройство.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StoredSession {
    pub server_url: String,
    pub server_id: String,
    pub user_id: String,
    pub login: String,
    pub device_id: String,
    pub access_token: String,
    /// Миллисекунды эпохи.
    pub access_token_expires_at: i64,
    pub refresh_token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreKind {
    Keyring,
    /// Связки ключей нет: файл `0600`.
    File,
}

pub struct SessionStore {
    fallback: PathBuf,
    /// Только файл (снимки окна, проверки): связку пользователя не трогаем.
    file_only: bool,
}

const LABEL: &str = "Melogold";

fn attributes() -> HashMap<&'static str, &'static str> {
    HashMap::from([("application", "app.melogold.Melogold"), ("kind", "session")])
}

impl SessionStore {
    pub fn new(fallback: PathBuf) -> SessionStore {
        SessionStore { fallback, file_only: false }
    }

    pub fn file_only(fallback: PathBuf) -> SessionStore {
        SessionStore { fallback, file_only: true }
    }

    async fn keyring(&self) -> Option<oo7::Keyring> {
        if self.file_only {
            return None;
        }
        match oo7::Keyring::new().await {
            Ok(keyring) => Some(keyring),
            Err(error) => {
                tracing::warn!(%error, "связки ключей нет: токены — в файле 0600");
                None
            }
        }
    }

    /// Сессия и где она лежит. Файл, оставшийся с тех пор, когда связки не было, переезжает в неё.
    pub async fn load(&self) -> (Option<StoredSession>, StoreKind) {
        if let Some(keyring) = self.keyring().await {
            let _ = keyring.unlock().await;
            let from_keyring = match keyring.search_items(&attributes()).await {
                Ok(items) => match items.first() {
                    Some(item) => item.secret().await.ok().and_then(|secret| serde_json::from_slice::<StoredSession>(&secret).ok()),
                    None => None,
                },
                Err(error) => {
                    tracing::warn!(%error, "сессия из связки ключей не прочиталась");
                    None
                }
            };
            if from_keyring.is_some() {
                return (from_keyring, StoreKind::Keyring);
            }
            if let Some(session) = self.read_file() {
                if self.save_keyring(&keyring, Some(&session)).await {
                    let _ = std::fs::remove_file(&self.fallback);
                }
                return (Some(session), StoreKind::Keyring);
            }
            return (None, StoreKind::Keyring);
        }
        (self.read_file(), StoreKind::File)
    }

    /// Записать (или стереть при `None`). Возвращает, где она теперь.
    pub async fn save(&self, session: Option<&StoredSession>) -> StoreKind {
        if let Some(keyring) = self.keyring().await {
            if self.save_keyring(&keyring, session).await {
                let _ = std::fs::remove_file(&self.fallback);
                return StoreKind::Keyring;
            }
        }
        self.write_file(session);
        StoreKind::File
    }

    async fn save_keyring(&self, keyring: &oo7::Keyring, session: Option<&StoredSession>) -> bool {
        let result = match session {
            Some(session) => {
                let json = serde_json::to_vec(session).unwrap_or_default();
                keyring.create_item(LABEL, &attributes(), json, true).await
            }
            None => keyring.delete(&attributes()).await,
        };
        match result {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "связка ключей не приняла сессию: токены — в файле 0600");
                false
            }
        }
    }

    fn read_file(&self) -> Option<StoredSession> {
        let text = std::fs::read(&self.fallback).ok()?;
        serde_json::from_slice(&text).ok()
    }

    fn write_file(&self, session: Option<&StoredSession>) {
        let Some(session) = session else {
            let _ = std::fs::remove_file(&self.fallback);
            return;
        };
        if let Some(parent) = self.fallback.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let json = serde_json::to_vec_pretty(session).unwrap_or_default();
        // Сначала права 0600, потом содержимое: токены не бывают видны другим даже мгновение.
        let temporary = self.fallback.with_extension("tmp");
        let written = (|| -> std::io::Result<()> {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&temporary)?;
            file.write_all(&json)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.fallback)
        })();
        if let Err(error) = written {
            tracing::warn!(%error, "сессия не записалась");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn file_store_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("melogold-session-{}", melogold_core::ids::new_uuid()));
        let store = SessionStore::file_only(dir.join("session.json"));
        assert_eq!(store.load().await, (None, StoreKind::File));
        let session = StoredSession { login: "e2e".into(), refresh_token: "mgrt1.a.b".into(), ..Default::default() };
        assert_eq!(store.save(Some(&session)).await, StoreKind::File);
        let mode = std::fs::metadata(dir.join("session.json")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(store.load().await.0, Some(session));
        store.save(None).await;
        assert_eq!(store.load().await.0, None);
        let _ = std::fs::remove_dir_all(dir);
    }
}

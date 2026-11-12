//! Хранение токенов в ОС-хранилище (ТЗ А4, НФР §8): macOS Keychain /
//! Windows Credential Manager / Secret Service. Токены и пароли **никогда**
//! не пишутся в конфиги и логи (protocol.md §4).
//!
//! В M0 — только smoke-тест DoD; публичный API задействуется с M1 (ФР-01,
//! экран входа и `auth.resume` — `transport` использует refresh-токен).

const SERVICE: &str = "ld2.client";

/// Ключ записи: `<host>:<login>` (одна запись на аккаунт/хост).
fn entry_key(host: &str, login: &str) -> String {
    format!("{host}:{login}")
}

pub fn set_token(host: &str, login: &str, token: &str) -> keyring::Result<()> {
    let entry = keyring::Entry::new(SERVICE, &entry_key(host, login))?;
    entry.set_password(token)
}

pub fn get_token(host: &str, login: &str) -> keyring::Result<String> {
    let entry = keyring::Entry::new(SERVICE, &entry_key(host, login))?;
    entry.get_password()
}

pub fn delete_token(host: &str, login: &str) -> keyring::Result<()> {
    let entry = keyring::Entry::new(SERVICE, &entry_key(host, login))?;
    entry.delete_credential()
}

// ─── M16/F29j: «запомнить пароль» — отдельная запись от токена ──────────────

const PASSWORD_SERVICE: &str = "ld2.client.password";

pub fn set_password(host: &str, login: &str, password: &str) -> keyring::Result<()> {
    let entry = keyring::Entry::new(PASSWORD_SERVICE, &entry_key(host, login))?;
    entry.set_password(password)
}

pub fn get_password(host: &str, login: &str) -> keyring::Result<String> {
    let entry = keyring::Entry::new(PASSWORD_SERVICE, &entry_key(host, login))?;
    entry.get_password()
}

/// Удалить запомненный пароль; отсутствие записи — не ошибка.
pub fn delete_password(host: &str, login: &str) -> keyring::Result<()> {
    let entry = keyring::Entry::new(PASSWORD_SERVICE, &entry_key(host, login))?;
    match entry.delete_credential() {
        Err(keyring::Error::NoEntry) => Ok(()),
        other => other,
    }
}

/// Чтение записи с таймаутом: зависший диалог доступа macOS не должен
/// блокировать запуск приложения (инциденты M4/M5). Общий для токена
/// и запомненного пароля (D12).
fn get_with_timeout(
    service: &str,
    host: &str,
    login: &str,
    timeout: std::time::Duration,
) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let service = service.to_string();
    let host = host.to_string();
    let login = login.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(
            keyring::Entry::new(&service, &entry_key(&host, &login))
                .and_then(|entry| entry.get_password()),
        );
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(secret)) => Some(secret),
        Ok(Err(keyring::Error::NoEntry)) => {
            tracing::debug!("keyring: записи нет — нужен вход");
            None
        }
        Ok(Err(e)) => {
            tracing::warn!("keyring: ошибка чтения: {e}");
            None
        }
        Err(_) => {
            tracing::warn!(
                "keyring: нет ответа за {timeout:?} — стартую без чтения \
                 (диалог доступа macOS отложен)"
            );
            None
        }
    }
}

/// Чтение токена с таймаутом (D12).
pub fn get_token_with_timeout(
    host: &str,
    login: &str,
    timeout: std::time::Duration,
) -> Option<String> {
    get_with_timeout(SERVICE, host, login, timeout)
}

/// Чтение запомненного пароля с таймаутом (M16/F29j, паритет токена).
pub fn get_password_with_timeout(
    host: &str,
    login: &str,
    timeout: std::time::Duration,
) -> Option<String> {
    get_with_timeout(PASSWORD_SERVICE, host, login, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke-тест DoD M0: запись → чтение → удаление в keychain.
    /// В окружениях без доступа к хранилищу (headless CI) — пропуск с пометкой.
    #[test]
    fn keyring_smoke() {
        let host = "ws://127.0.0.1:24817/ws";
        let login = format!("smoke-{}", std::process::id());

        match set_token(host, &login, "test-token-123") {
            Ok(()) => {
                assert_eq!(
                    get_token(host, &login).expect("чтение токена"),
                    "test-token-123"
                );
                delete_token(host, &login).expect("удаление токена");
                assert!(
                    get_token(host, &login).is_err(),
                    "после удаления чтение должно падать"
                );
                // M16/F29j: запомненный пароль — отдельная запись.
                set_password(host, &login, "pw-123").expect("запись пароля");
                assert_eq!(get_password(host, &login).expect("чтение пароля"), "pw-123");
                delete_password(host, &login).expect("удаление пароля");
                assert!(
                    get_password(host, &login).is_err(),
                    "пароль удалён — чтение падает"
                );
                tracing::info!("keyring smoke: ok");
            }
            Err(e) => {
                eprintln!("keyring smoke ПРОПУСК: окружение без хранилища ключей ({e})");
            }
        }
    }

    /// M10.3: обёртка чтения keyring обязана вернуться за таймаут даже при
    /// зависшем диалоге доступа (инцидент M4/M5).
    #[test]
    fn get_token_with_timeout_returns_none_for_missing_entry() {
        let start = std::time::Instant::now();
        let token = get_token_with_timeout(
            "ws://test-never:1/ws",
            "no-such-login",
            std::time::Duration::from_secs(2),
        );
        assert!(token.is_none(), "нет записи → None");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(3),
            "обёртка не зависает: {:?}",
            start.elapsed()
        );
    }
}

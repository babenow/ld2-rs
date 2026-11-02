//! Хранение токенов в ОС-хранилище (ТЗ А4, НФР §8): macOS Keychain /
//! Windows Credential Manager / Secret Service. Токены и пароли **никогда**
//! не пишутся в конфиги и логи (protocol.md §4).
//!
//! В M0 — только smoke-тест DoD; публичный API задействуется с M1 (ФР-01,
//! экран входа и `auth.resume`).
#![allow(dead_code)] // API включается в M1 (ФР-01); в M0 используется только тестом.

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
                tracing::info!("keyring smoke: ok");
            }
            Err(e) => {
                eprintln!("keyring smoke ПРОПУСК: окружение без хранилища ключей ({e})");
            }
        }
    }
}

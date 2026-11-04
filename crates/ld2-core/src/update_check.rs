//! Проверка обновлений при старте (НФР «Обновления», §12): GitHub Releases
//! API, **не блокирующая** игру — фоновый поток, бейдж в header, «скачать»
//! ведёт в браузер. Принудительная минималка — capability сервера
//! `min_client` (обрабатывается отдельно в сторе).

use std::time::Duration;

/// Репозиторий релизов (секретов нет — публичный API).
pub const RELEASES_URL: &str = "https://api.github.com/repos/babenow/ld2-rs/releases/latest";
/// Страница релизов для бейджа (открытие в браузере).
pub const RELEASES_PAGE: &str = "https://github.com/babenow/ld2-rs/releases";

/// Запуск проверки в фоне: результат — в канал (`Some(версия)` без `v`).
/// Сетевые ошибки молча игнорируются — обновление не критично для старта.
pub fn spawn_check(tx: std::sync::mpsc::Sender<String>) {
    std::thread::spawn(move || {
        if let Some(version) = latest_version() {
            let _ = tx.send(version);
        }
    });
}

/// `GET releases/latest` → версия из `tag_name` (без ведущей `v`).
pub fn latest_version() -> Option<String> {
    let resp = ureq::get(RELEASES_URL)
        .timeout(Duration::from_secs(5))
        .call()
        .ok()?;
    let text = resp.into_string().ok()?;
    parse_latest_version(&text)
}

/// Парсинг ответа GitHub API: `{"tag_name": "v0.10.0", ...}`.
pub fn parse_latest_version(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let tag = json["tag_name"].as_str()?;
    Some(tag.trim_start_matches('v').to_string())
}

/// Сравнение semver-строк `a > b` (по трём числовым компонентам;
/// нечисловые суффиксы игнорируются — `0.9.0-rc.1` ведёт себя как `0.9.0`).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> [u64; 3] {
        let mut out = [0u64; 3];
        for (i, part) in v.split(['.', '-']).take(3).enumerate() {
            out[i] = part.parse().unwrap_or(0);
        }
        out
    }
    parts(candidate) > parts(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_tag_name() {
        let body = r#"{"html_url":"x","tag_name":"v0.10.0","name":"v0.10.0"}"#;
        assert_eq!(parse_latest_version(body).as_deref(), Some("0.10.0"));
        assert!(parse_latest_version("{}").is_none(), "нет tag_name");
        assert!(parse_latest_version("не json").is_none());
    }

    #[test]
    fn semver_comparison() {
        assert!(is_newer("0.10.0", "0.9.0"), "minor растёт");
        assert!(is_newer("1.0.0", "0.9.9"), "major растёт");
        assert!(is_newer("0.9.1", "0.9.0"), "patch растёт");
        assert!(!is_newer("0.9.0", "0.9.0"), "равно — не новое");
        assert!(!is_newer("0.8.9", "0.9.0"), "старее — не показываем");
    }
}

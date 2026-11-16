//! M19 (ADR-013, `docs/protocol.md` §15): резолв **домена** →
//! well-known-документ → адрес WS + требования к клиенту.
//!
//! Вызывается клиентом **до** подключения (F19c — из `spawn_blocking`:
//! [`ureq`] блокирует поток). Смена адреса сервера не требует перевыпуска
//! клиента; ошибки — [`ResolveError`] (понятные тексты → `store.notice`).

use std::time::Duration;

use ld2_protocol::DiscoveryDoc;

/// Таймаут запроса документа (маленький: это первый шаг подключения).
const DOC_TIMEOUT: Duration = Duration::from_secs(5);

/// Ошибки резолва (protocol.md §15; тексты → l10n клиента по коду).
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// Пустой/некорректный домен (не URL и не «host[:port]»).
    #[error("некорректный домен: {0}")]
    BadDomain(String),
    /// Сервер/сеть недоступны или таймаут.
    #[error("документ не получен ({0})")]
    Unreachable(String),
    /// Ответ — не JSON / без обязательного `address`.
    #[error("некорректный документ: {0}")]
    BadBody(String),
    /// `version` ≠ версии контракта клиента — «обновите клиент».
    #[error("контракт требует обновления клиента (сервер v{server}, клиент v{client})")]
    IncompatibleContract { server: u32, client: u32 },
    /// `min_client` новее версии клиента — «нужно обновление».
    #[error("нужно обновление клиента (нужно {need}, у вас {have})")]
    MinClient { need: String, have: String },
    /// `address` не проходит guard (§8): не `ws(s)://` либо `ws://` вне
    /// localhost в release-сборке.
    #[error("небезопасный адрес сервера: {0}")]
    InsecureAddress(String),
}

/// URL документа для домена (protocol.md §15):
/// `https://<домен>/.well-known/ld2.json`; в dev-сборке для localhost
/// допустим `http://`. Домен может прийти и со схемой (`https://…`) —
/// тогда путь дописывается к нему (http-схема — только вне release).
pub fn doc_url(domain: &str, release: bool) -> Result<String, ResolveError> {
    let trimmed = domain.trim().trim_end_matches('/');
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return Err(ResolveError::BadDomain(domain.to_string()));
    }
    let base = if let Some(rest) = trimmed.strip_prefix("https://") {
        if rest.is_empty() {
            return Err(ResolveError::BadDomain(domain.to_string()));
        }
        format!("https://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        // http — только dev (localhost) и только вне release.
        if release || !is_local(rest) {
            return Err(ResolveError::InsecureAddress(trimmed.to_string()));
        }
        format!("http://{rest}")
    } else {
        // Чистый домен: https; dev-localhost — http (ADR-013: dev-режим
        // сервера отдаёт документ без TLS).
        if !release && is_local(trimmed) {
            format!("http://{trimmed}")
        } else {
            format!("https://{trimmed}")
        }
    };
    Ok(format!("{base}/.well-known/ld2.json"))
}

/// localhost-проверка для http-фолбэка (`host[:port]`, IPv6-скобки).
fn is_local(hostport: &str) -> bool {
    let host = hostport.split_once(':').map(|(h, _)| h).unwrap_or(hostport);
    let host = host.trim_matches(|c| c == '[' || c == ']');
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// Сравнение semver **строго по >=** (ADR-013: подтверждено заказчиком):
/// недостающие сегменты — 0; нечисловые сегменты (pre-release и пр.) — 0
/// (`0.18.0-rc1` как `0.18.0` — приемлемо для гейта минимума).
pub fn semver_at_least(have: &str, need: &str) -> bool {
    fn segs(v: &str) -> [u64; 3] {
        let mut out = [0u64; 3];
        for (i, part) in v.split('.').enumerate().take(3) {
            let num = part
                .split(['-', '+'])
                .next()
                .unwrap_or("0")
                .parse::<u64>()
                .unwrap_or(0);
            out[i] = num;
        }
        out
    }
    segs(have) >= segs(need)
}

/// Разбор тела ответа: JSON → [`DiscoveryDoc`] (без `address` — ошибка).
pub fn parse_doc(body: &str) -> Result<DiscoveryDoc, ResolveError> {
    let doc: DiscoveryDoc =
        serde_json::from_str(body).map_err(|e| ResolveError::BadBody(e.to_string()))?;
    if doc.address.trim().is_empty() {
        return Err(ResolveError::BadBody("нет поля address".into()));
    }
    Ok(doc)
}

/// Валидация документа (protocol.md §15): версия контракта, min_client,
/// guard адреса (§8: внешний `ws://` в release запрещён).
pub fn validate_doc(doc: DiscoveryDoc, release: bool) -> Result<DiscoveryDoc, ResolveError> {
    let client_version = env!("CARGO_PKG_VERSION");
    if doc.version != DiscoveryDoc::expected_version() {
        return Err(ResolveError::IncompatibleContract {
            server: doc.version,
            client: DiscoveryDoc::expected_version(),
        });
    }
    if let Some(need) = doc.min_client.as_deref() {
        if !semver_at_least(client_version, need) {
            return Err(ResolveError::MinClient {
                need: need.to_string(),
                have: client_version.to_string(),
            });
        }
    }
    // Guard адреса (паритет host_allowed, config.rs): только ws(s)://,
    // в release внешний ws:// запрещён (только localhost).
    let address = doc.address.trim();
    if let Some(rest) = address.strip_prefix("ws://") {
        if release && !is_local(rest) {
            return Err(ResolveError::InsecureAddress(address.to_string()));
        }
    } else if !address.starts_with("wss://") {
        return Err(ResolveError::InsecureAddress(address.to_string()));
    }
    Ok(doc)
}

/// Полный резолв: URL → GET (блокирующий! — вызывать из
/// `spawn_blocking`) → разбор → валидация.
pub fn resolve_blocking(domain: &str, release: bool) -> Result<DiscoveryDoc, ResolveError> {
    let url = doc_url(domain, release)?;
    let response = ureq::get(&url)
        .timeout(DOC_TIMEOUT)
        .call()
        .map_err(|e| ResolveError::Unreachable(e.to_string()))?;
    let body = response
        .into_string()
        .map_err(|e| ResolveError::Unreachable(e.to_string()))?;
    let doc = parse_doc(&body)?;
    validate_doc(doc, release)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── doc_url ────────────────────────────────────────────────────────

    #[test]
    fn doc_url_uses_https_for_domains() {
        assert_eq!(
            doc_url("game.example.com", true).expect("url"),
            "https://game.example.com/.well-known/ld2.json"
        );
        // Схема у дозволенного домена — https (release и dev).
        assert_eq!(
            doc_url("https://game.example.com/", false).expect("url"),
            "https://game.example.com/.well-known/ld2.json"
        );
    }

    #[test]
    fn doc_url_dev_localhost_uses_http() {
        assert_eq!(
            doc_url("127.0.0.1:24817", false).expect("url"),
            "http://127.0.0.1:24817/.well-known/ld2.json"
        );
        assert_eq!(
            doc_url("localhost", false).expect("url"),
            "http://localhost/.well-known/ld2.json"
        );
        // Release: localhost тоже https (TLS — на стороне прокси/инвариант).
        assert_eq!(
            doc_url("localhost", true).expect("url"),
            "https://localhost/.well-known/ld2.json"
        );
    }

    #[test]
    fn doc_url_rejects_insecure_and_empty() {
        // http в release — только https.
        assert!(matches!(
            doc_url("http://evil.example", true),
            Err(ResolveError::InsecureAddress(_))
        ));
        // http наружу и в dev запрещён (только localhost).
        assert!(matches!(
            doc_url("http://evil.example", false),
            Err(ResolveError::InsecureAddress(_))
        ));
        assert!(matches!(
            doc_url("   ", false),
            Err(ResolveError::BadDomain(_))
        ));
        assert!(matches!(
            doc_url("bad host", false),
            Err(ResolveError::BadDomain(_))
        ));
    }

    // ── semver ─────────────────────────────────────────────────────────

    #[test]
    fn semver_at_least_compares_major_minor_patch() {
        assert!(semver_at_least("0.17.0", "0.17.0"), "равно — проходит");
        assert!(semver_at_least("0.18.0", "0.17.9"), "новее — проходит");
        assert!(semver_at_least("1.0.0", "0.99.99"));
        assert!(!semver_at_least("0.17.0", "0.18.0"), "старше — отказ");
        assert!(!semver_at_least("0.9.0", "0.10.0"), "сравнение числовое");
        // Недостающие сегменты — 0; нечисловые — 0.
        assert!(semver_at_least("0.18", "0.18.0"));
        assert!(!semver_at_least("0.17.0-rc1", "0.17.1"));
    }

    // ── parse ──────────────────────────────────────────────────────────

    #[test]
    fn parse_requires_json_with_address() {
        assert!(parse_doc("{\"version\":1}").is_err(), "нет address");
        assert!(parse_doc("не json").is_err(), "не JSON");
        assert!(parse_doc("{\"address\":\"  \"}").is_err(), "пустой адрес");
        let doc = parse_doc(r#"{"address":"wss://a/ws","version":1}"#).expect("ок");
        assert_eq!(doc.address, "wss://a/ws");
    }

    // ── validate ───────────────────────────────────────────────────────

    #[test]
    fn validate_rejects_wrong_contract_version() {
        let doc = parse_doc(r#"{"address":"wss://a/ws","version":2}"#).expect("ок");
        assert!(matches!(
            validate_doc(doc, false),
            Err(ResolveError::IncompatibleContract { server: 2, .. })
        ));
    }

    #[test]
    fn validate_gates_min_client() {
        // Клиент этой сборки ≥0.18? Нет (0.17) → «нужно обновление».
        let doc = parse_doc(&format!(
            r#"{{"address":"wss://a/ws","version":1,"min_client":"{}.99.0"}}"#,
            u64::MAX
        ))
        .expect("ок");
        assert!(matches!(
            validate_doc(doc, false),
            Err(ResolveError::MinClient { .. })
        ));
        // Минимум ≤ версии клиента — проходит.
        let client = env!("CARGO_PKG_VERSION");
        let doc = parse_doc(&format!(
            r#"{{"address":"wss://a/ws","version":1,"min_client":"{client}"}}"#
        ))
        .expect("ок");
        assert!(
            validate_doc(doc, false).is_ok(),
            "min_client == версии — ок"
        );
    }

    #[test]
    fn validate_guards_address() {
        // Не ws(s):// — отказ.
        let doc = parse_doc(r#"{"address":"https://a/ws","version":1}"#).expect("ок");
        assert!(matches!(
            validate_doc(doc, false),
            Err(ResolveError::InsecureAddress(_))
        ));
        // Release: внешний ws:// запрещён (§8).
        let doc = parse_doc(r#"{"address":"ws://a.example/ws","version":1}"#).expect("ок");
        assert!(matches!(
            validate_doc(doc, true),
            Err(ResolveError::InsecureAddress(_))
        ));
        // Dev: localhost ws:// допустим; wss:// — всегда.
        let doc = parse_doc(r#"{"address":"ws://127.0.0.1:24817/ws","version":1}"#).expect("ок");
        assert!(validate_doc(doc, false).is_ok(), "dev localhost ws");
        let doc = parse_doc(r#"{"address":"wss://a.example/ws","version":1}"#).expect("ок");
        assert!(validate_doc(doc, true).is_ok(), "release wss");
    }
}

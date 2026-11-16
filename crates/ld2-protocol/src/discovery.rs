//! M19 (ADR-013, `docs/protocol.md` §15): well-known discovery-документ
//! — `GET https://<домен>/.well-known/ld2.json` → адрес WS-эндпоинта и
//! требования к клиенту. **Вне WS-контракта** (не `[tz-amend]`): документ
//! читается клиентом до подключения; незнакомые ключи игнорируются (§11).

use serde::{Deserialize, Serialize};

use crate::envelope::PROTOCOL_VERSION;
use crate::types::JsonMap;

/// Документ discovery (protocol.md §15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryDoc {
    /// WS-эндпоинт (`ws://`/`wss://`) — единственный обязательный ключ.
    pub address: String,
    /// Версия WS-контракта (`v`); устаревшая/новее — клиент отказывается.
    pub version: u32,
    /// Опциональная минимальная версия клиента (semver, §5 `min_client`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_client: Option<String>,
    /// §11: незнакомые ключи — игнорируются при чтении.
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

impl DiscoveryDoc {
    /// Версия контракта, которую ждёт этот клиент.
    pub const fn expected_version() -> u32 {
        PROTOCOL_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Разбор полного документа + игнорирование незнакомых ключей (§11).
    #[test]
    fn doc_parses_full_and_ignores_unknown() {
        let doc: DiscoveryDoc = serde_json::from_str(
            r#"{"address":"wss://game.example.com/ws","version":1,
                "min_client":"0.18.0","future_key":{"x":1}}"#,
        )
        .expect("разбор");
        assert_eq!(doc.address, "wss://game.example.com/ws");
        assert_eq!(doc.version, 1);
        assert_eq!(doc.min_client.as_deref(), Some("0.18.0"));
        assert!(doc.extra.contains_key("future_key"), "§11: extra сохранён");
    }

    /// Без min_client и без лишних ключей — документ валиден.
    #[test]
    fn doc_parses_minimal() {
        let doc: DiscoveryDoc =
            serde_json::from_str(r#"{"address":"ws://127.0.0.1:24817/ws","version":1}"#)
                .expect("разбор");
        assert_eq!(doc.min_client, None);
        assert!(doc.extra.is_empty());
    }

    /// Отсутствие обязательного address — ошибка разбора.
    #[test]
    fn doc_without_address_is_rejected() {
        assert!(serde_json::from_str::<DiscoveryDoc>(r#"{"version":1}"#).is_err());
    }

    /// round-trip: сериализация не теряет поля.
    #[test]
    fn doc_round_trip() {
        let doc = DiscoveryDoc {
            address: "wss://a.example/ws".into(),
            version: 1,
            min_client: Some("0.18.0".into()),
            extra: JsonMap::new(),
        };
        let json = serde_json::to_string(&doc).expect("сериализация");
        let back: DiscoveryDoc = serde_json::from_str(&json).expect("разбор");
        assert_eq!(back, doc);
    }
}

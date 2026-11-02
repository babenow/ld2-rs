//! Конверт сообщений: `docs/protocol.md` §2.
//!
//! Один текстовый WS-фрейм = ровно один JSON-объект. Варианты:
//! - команда `cmd/...` (клиент → сервер): `seq`, `ts`, `payload`;
//! - подтверждение `ack` (сервер → клиент): эхо `seq`, `result`, `state_version`;
//! - событие `evt/...` (сервер → клиент): `ts`, `state_version` (опц.), `payload`.

use serde::{Deserialize, Serialize};

/// Текущая wire-версия контракта (`docs/protocol.md` §11).
pub const PROTOCOL_VERSION: u32 = 1;

/// Максимальный размер сообщения (64 KiB, protocol.md §1).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Универсальный конверт. Типизированные payload'ы появятся в M1;
/// в M0 `payload` — произвольный JSON (заглушка).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Версия протокола; клиент МУСТ игнорировать незнакомые `v > 1` (кроме §11).
    pub v: u32,
    /// `cmd/<domain>.<action>` либо `evt/<domain>.<event>`.
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Только команды: монотонный u32 (wrapping), эхо в `ack`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u32>,
    /// ISO-8601 UTC; игровая истина времени — epoch-секунды в payload'ах.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

impl Envelope {
    pub fn new(msg_type: impl Into<String>, payload: Option<serde_json::Value>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            msg_type: msg_type.into(),
            seq: None,
            ts: None,
            payload,
        }
    }

    /// Сериализация с проверкой лимита 64 KiB (protocol.md §1).
    pub fn to_json_limited(&self) -> Result<String, crate::errors::ProtocolError> {
        let json = serde_json::to_string(self)?;
        if json.len() > MAX_MESSAGE_BYTES {
            return Err(crate::errors::ProtocolError::TooLarge(json.len()));
        }
        Ok(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden round-trip: контракт не должен терять/переименовывать поля (protocol.md §13).
    #[test]
    fn envelope_round_trip() {
        let original = Envelope {
            v: PROTOCOL_VERSION,
            msg_type: "cmd/location.move".into(),
            seq: Some(42),
            ts: Some("2026-11-03T12:00:00Z".into()),
            payload: Some(serde_json::json!({ "exit_id": "e_north" })),
        };
        let json = original.to_json_limited().expect("сериализация");
        assert!(json.contains("\"v\":1"));
        assert!(json.contains("\"type\":\"cmd/location.move\""));
        let parsed: Envelope = serde_json::from_str(&json).expect("десериализация");
        assert_eq!(original, parsed);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // Клиент МУСТ переживать незнакомые поля (protocol.md §2.3, §11).
        let json = r#"{"v":1,"type":"evt/whatever","ts":"2026-11-03T12:00:00Z",
                       "payload":{},"brand_new_field":123}"#;
        let parsed: Envelope = serde_json::from_str(json).expect("незнакомые поля игнорируются");
        assert_eq!(parsed.v, 1);
        assert_eq!(parsed.msg_type, "evt/whatever");
    }

    #[test]
    fn size_limit_enforced() {
        let big = Envelope::new(
            "cmd/test",
            Some(serde_json::json!({ "blob": "x".repeat(70_000) })),
        );
        assert!(matches!(
            big.to_json_limited(),
            Err(crate::errors::ProtocolError::TooLarge(_))
        ));
    }
}

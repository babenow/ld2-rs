//! Конверт сообщений: `docs/protocol.md` §2.
//!
//! Один текстовый WS-фрейм = ровно один JSON-объект. Варианты:
//! - команда `cmd/...` (клиент → сервер): `seq`, `ts`, `payload`;
//! - подтверждение `ack` (сервер → клиент): эхо `seq`, `result`, `state_version`;
//! - событие `evt/...` (сервер → клиент): `ts`, `state_version` (опц.), `payload`.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::cmd::Command;
use crate::errors::{ErrorCode, ProtocolError};

/// Текущая wire-версия контракта (`docs/protocol.md` §11).
pub const PROTOCOL_VERSION: u32 = 1;

/// Максимальный размер сообщения (64 KiB, protocol.md §1).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Wire-тип подтверждения (§2.2).
pub const ACK_TYPE: &str = "ack";

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
    /// Версия состояния, которой соответствует событие (§2.3): клиент
    /// игнорирует события с версией < уже применённой.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_version: Option<u64>,
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
            state_version: None,
            payload,
        }
    }

    /// Сериализация с проверкой лимита 64 KiB (protocol.md §1).
    pub fn to_json_limited(&self) -> Result<String, ProtocolError> {
        let json = serde_json::to_string(self)?;
        if json.len() > MAX_MESSAGE_BYTES {
            return Err(ProtocolError::TooLarge(json.len()));
        }
        Ok(json)
    }

    /// Оборачивает команду в конверт: `v = 1`, `type = C::TYPE`, `seq`, `ts`.
    pub fn command<C: Command>(
        seq: u32,
        ts: impl Into<String>,
        payload: &C,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            v: PROTOCOL_VERSION,
            msg_type: C::TYPE.to_string(),
            seq: Some(seq),
            ts: Some(ts.into()),
            state_version: None,
            payload: Some(serde_json::to_value(payload)?),
        })
    }

    /// Десериализация payload'а в типизированную структуру команды/события.
    pub fn payload_as<T: DeserializeOwned>(&self) -> Result<T, ProtocolError> {
        match &self.payload {
            Some(value) => Ok(serde_json::from_value(value.clone())?),
            None => Err(ProtocolError::MissingPayload),
        }
    }
}

/// Подтверждение команды (protocol.md §2.2): ровно одно на команду.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ack {
    pub v: u32,
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Эхо `seq` команды.
    pub seq: u32,
    /// Версия состояния ПОСЛЕ команды; `null` — состояние не менялось.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_version: Option<u64>,
    pub result: AckResult,
    /// Результат команды (новая локация, список персонажей, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AckError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckResult {
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AckError {
    /// `E_...` из §9; неизвестный код → [`ErrorCode::Unknown`] (§11).
    pub code: String,
    /// Русский текст сервера — фолбэк отображения (§9).
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_s: Option<u64>,
}

impl Ack {
    pub fn ok(seq: u32, state_version: Option<u64>) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            msg_type: ACK_TYPE.to_string(),
            seq,
            state_version,
            result: AckResult::Ok,
            payload: None,
            error: None,
        }
    }

    pub fn ok_with_payload<T: Serialize>(
        seq: u32,
        state_version: Option<u64>,
        payload: &T,
    ) -> Result<Self, ProtocolError> {
        let mut ack = Self::ok(seq, state_version);
        ack.payload = Some(serde_json::to_value(payload)?);
        Ok(ack)
    }

    pub fn error(
        seq: u32,
        state_version: Option<u64>,
        code: &ErrorCode,
        message: impl Into<String>,
    ) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            msg_type: ACK_TYPE.to_string(),
            seq,
            state_version,
            result: AckResult::Error,
            payload: None,
            error: Some(AckError {
                code: code.as_str().to_string(),
                message: message.into(),
                retry_after_s: None,
            }),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.result == AckResult::Ok
    }

    /// Код ошибки из ack; `None` если `result = ok`.
    pub fn error_code(&self) -> Option<ErrorCode> {
        let error = self.error.as_ref()?;
        Some(
            error
                .code
                .parse()
                .unwrap_or_else(|_| ErrorCode::Unknown(error.code.clone())),
        )
    }

    pub fn payload_as<T: DeserializeOwned>(&self) -> Result<T, ProtocolError> {
        match &self.payload {
            Some(value) => Ok(serde_json::from_value(value.clone())?),
            None => Err(ProtocolError::MissingPayload),
        }
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
            state_version: None,
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

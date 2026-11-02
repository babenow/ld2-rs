//! Ошибки протокола: `docs/protocol.md` §9 (каталог кодов) и ошибки кодирования.

use thiserror::Error;

/// Ошибки уровня сериализации/лимитов конверта (не игровые `error.code`).
#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("сериализация/десериализация: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("сообщение превышает лимит 64 KiB: {0} байт")]
    TooLarge(usize),
}

/// Игровые коды ошибок `ack.error.code` (protocol.md §9).
///
/// M0 — только заготовка перечисления; полный набор и локализация — M1.
/// Клиент обязан иметь русский текст на каждый код; поле `message` сервера — фолбэк.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    AuthExpired,
    NotEnoughMp,
    NoCharges,
    RateLimited,
    Internal,
}

impl ErrorCode {
    /// Строковый код в wire-формате (`E_...`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthExpired => "E_AUTH_EXPIRED",
            Self::NotEnoughMp => "E_NOT_ENOUGH_MP",
            Self::NoCharges => "E_NO_CHARGES",
            Self::RateLimited => "E_RATE_LIMITED",
            Self::Internal => "E_INTERNAL",
        }
    }
}

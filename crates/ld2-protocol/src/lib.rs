//! # ld2-protocol
//!
//! Серийные типы контракта «клиент ↔ сервер» (нормативный документ — `docs/protocol.md`).
//!
//! Правила crate'а (ТЗ §9, protocol.md §11):
//! - **никаких** зависимостей от UI (`eframe`) и транспорта (`tokio`);
//! - несовместимые изменения контракта = bump wire-версии `v` + PR `[tz-amend]` + golden-тесты;
//! - аддитивные поля — совместимы, клиент обязан игнорировать незнакомые ключи/типы.
//!
//! Объём M1 (v0.1 «Нить»): полный набор `cmd/*` и `evt/*`, коды ошибок, capabilities.
//! В M0 — только конверт-заглушка.

pub mod cmd;
pub mod envelope;
pub mod errors;
pub mod evt;
pub mod features;
pub mod spells;
pub mod types;

pub use cmd::Command;
pub use envelope::{
    Ack, AckError, AckResult, Envelope, ACK_TYPE, MAX_MESSAGE_BYTES, PROTOCOL_VERSION,
};
pub use errors::{ErrorCode, ProtocolError};
pub use evt::Event;
pub use features::SessionConfig;
pub use types::{
    CharacterState, CrimeStatus, Entity, Hp, ItemSnapshot, LocationSnapshot, Mp, SkillSnapshot,
};

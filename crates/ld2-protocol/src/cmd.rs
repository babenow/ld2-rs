//! Команды `cmd/*` — `docs/protocol.md` §7 (таблица payload/ack/idempotency).
//!
//! **Объём M1:** типизированные структуры всех команд (`auth.*`, `char.*`,
//! `location.*`, `combat.*`, `item.*`, `magic.*`, `socket.*`, `upgrade.*`,
//! `craft.*`, `quest.*`, `crime.*`, `chat.*`, `party.*`, `trade.*`, `guild.*`,
//! `state.resync`, `ping`, `time.sync`) с `serde`-типами и golden-тестами.
//!
//! В M0 — заглушка: команды уходят как `Envelope` с произвольным payload.

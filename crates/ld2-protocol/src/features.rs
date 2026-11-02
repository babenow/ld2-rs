//! Capabilities и конфиг аккаунта `evt/session.start` — `docs/protocol.md` §5.
//!
//! **Объём M1:** структура `SessionConfig` (`char_slot_limit`, `profession_limit`,
//! `features`, `crime.*`, `chat.*`, `inventory.*`, `upgrade.*`, `magic.mana`, …).
//!
//! Правило (ТЗ §15.4): клиент берёт все лимиты/фичи отсюда; незнакомые ключи
//! игнорируются, хардкоды вида `MAX_CHARS` запрещены.

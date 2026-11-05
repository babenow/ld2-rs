//! Реальный сервер ld2 (PLAN-M12): PostgreSQL, конфиг из `server.toml` (D25).
//!
//! M12.1 (P0): каркас — конфиг с валидацией (fail-fast), пул sqlx + миграции
//! (`migrations/*.sql`, D24), `/healthz`, graceful shutdown. M12.2 (P1):
//! WS-транспорт §1/§2 (seq-окно, ack), авторизация и персонажи; поведение
//! контракта — `docs/protocol.md`.

pub mod app;
pub mod config;
pub mod db;
pub mod session;
pub mod state;
pub mod time;
pub mod transport;

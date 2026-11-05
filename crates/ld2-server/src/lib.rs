//! Реальный сервер ld2 (PLAN-M12): PostgreSQL, конфиг из `server.toml` (D25).
//!
//! M12.1 (P0): каркас — конфиг с валидацией (fail-fast), пул sqlx + миграции
//! (`migrations/*.sql`, D24), `/healthz`, graceful shutdown. WS-транспорт и
//! игровая логика — фазы M12.2+; поведение контракта — `docs/protocol.md`.

pub mod app;
pub mod config;
pub mod db;

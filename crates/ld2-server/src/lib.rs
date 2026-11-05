//! Реальный сервер ld2 (PLAN-M12): PostgreSQL, конфиг из `server.toml` (D25).
//!
//! M12.1 (P0): каркас — конфиг с валидацией (fail-fast), пул sqlx + миграции
//! (`migrations/*.sql`, D24), `/healthz`, graceful shutdown. M12.2 (P1):
//! WS-транспорт §1/§2 (seq-окно, ack), авторизация (ADR-006) и персонажи;
//! поведение контракта — `docs/protocol.md`.

pub mod app;
pub mod auth;
pub mod config;
pub mod content;
pub mod db;
pub mod death;
pub mod session;
pub mod social;
pub mod state;
pub mod systems;
pub mod tick;
pub mod time;
pub mod transport;
pub mod world;

//! # ld2-mock-server
//!
//! Детерминированный мок-сервер ld2: держит контракт `docs/protocol.md`
//! (cmd → ack c `state_version`, события после ack) и сценарии PLAN-M1 T3:
//!
//! - **hello** — `cmd/auth.login` (demo/demo123) → `ack` + `evt/session.start`
//!   с конфигом §5 (включая D5-kinds и D4-ману);
//! - **snapshot** — `cmd/char.select` → полный `evt/state/snapshot` (ТЗ §4.8.2),
//!   далее мир: две локации, бой, предметы, руны/мана, крафт, квесты;
//! - **chaos** — детерминированные случайные обрывы WS (seed, xorshift);
//! - `cmd/ping` → `ack` + `evt/pong`, `cmd/time.sync` → `evt/time.sync`.
//!
//! Фикстуры — из канонических примеров документации (см. [`fixtures`]).
//! Сквозные e2e-тесты управляют сервером через [`Knobs`].

pub mod chaos;
pub mod fixtures;
pub mod knobs;
pub mod server;
pub mod session;

pub use chaos::Chaos;
pub use knobs::Knobs;
pub use server::{serve, serve_configured, serve_with_knobs, serve_with_listener, MockOptions};

//! # ld2-core
//!
//! Ядро десктоп-клиента ld2 — **без UI-зависимостей** (D12, PLAN-M10):
//!
//! - [`store`] — `AppStore` + редукторы (собирается только из событий);
//! - [`transport`] — WebSocket, `seq`/ack, reconnect/resync (свой tokio-runtime);
//! - [`l10n`] — таблицы ru/en;
//! - [`config`] — `config.toml`, guard release-сборки, горячие клавиши;
//! - [`register`] — форма регистрации и её валидация (M13, D34);
//! - [`secrets`] — keyring-токены;
//! - [`notify`] — OS-уведомления (D7);
//! - [`update_check`] — проверка GitHub Releases (§12).
//!
//! Отсюда зависит десктоп-клиент (`ld2-client`, бинарь `ld2-gpui`) —
//! логика игры и тесты общие.

pub mod config;
pub mod l10n;
pub mod logging;
pub mod notify;
pub mod register;
pub mod secrets;
pub mod store;
pub mod transport;
pub mod update_check;

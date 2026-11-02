//! `AppStore` — единое состояние клиента, собираемое ТОЛЬКО из событий сервера
//! (ТЗ §6.2, `docs/tz.md`).
//!
//! **Объём M1–M2:** `session`, `config`, `chars`, `snapshot{location,char,party,guild}`,
//! `crime`, `quests`, `chat[per-channel ring]`, `trade`, `ui_state`, `log` +
//! чистые редукторы `reducer/*.rs` (тестируются без egui).
//!
//! В M0 — заглушка: UI читает только `config`.

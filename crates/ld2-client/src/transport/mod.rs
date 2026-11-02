//! Транспортный слой: WebSocket, `seq`/`ack`, backoff-reconnect, resync
//! (ТЗ §6.3, `docs/protocol.md` §2.4, §3).
//!
//! **Объём M1:** `tokio-tungstenite`-клиент, очередь исходящих команд с `seq`,
//! таймаут ack 10 с, идемпотентные ретраи, `auth.resume` → `state.resync`,
//! индикатор статуса `online/reconnecting/offline/auth_expired`.
//!
//! В M0 — заглушка: адрес берётся из `config.host` (мок на 24817).

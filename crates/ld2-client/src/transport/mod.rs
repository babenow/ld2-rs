//! Транспортный слой клиента: WebSocket, `seq`/ack, backoff-reconnect,
//! resume → resync (ТЗ §6.3, `docs/protocol.md` §2.4, §3, §4.5; PLAN-M1 T2).
//!
//! Архитектура: отдельный поток `ld2-transport` со своим tokio-рантаймом;
//! общение с приложением через два mpsc-канала:
//!
//! ```text
//! App (eframe, главный поток)              Транспорт (поток + runtime)
//!   send<C: Command>()  ──Outbound──►        write-loop / backoff / resume
//!   ◄──IngressEvent────  ack/evt/status      read-loop / таймауты ack
//! ```
//!
//! Гарантии (§2.4): ровно один ack на команду; таймаут 10 с; повтор — только
//! идемпотентный и не более одного; при обрыве pending'и откатываются с
//! явным `CommandFailed` (ретраи через реконнект — решение приложения).

mod backoff;
mod connection;
mod queue;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ld2_protocol::cmd::Command;
use ld2_protocol::{Ack, Envelope};
use thiserror::Error;
use tokio::sync::mpsc;

/// Таймаут ack (protocol.md §2.4).
pub const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(10);
/// Старт backoff (ТЗ §6.3: 0.5 с → 30 с ±20%).
pub const DEFAULT_BACKOFF_BASE: Duration = Duration::from_millis(500);
pub const DEFAULT_BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Таймаут TCP/WS-подключения.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("транспорт остановлен — канал закрыт")]
    Stopped,
    #[error("ошибка сериализации команды: {0}")]
    Encode(#[from] ld2_protocol::ProtocolError),
}

/// Конфигурация транспорта. `refresh_token` — из keyring (ФР-01/А4);
/// при его наличии подключение всегда идёт через `auth.resume` → `state.resync`.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub host: String,
    pub refresh_token: Option<String>,
    pub ack_timeout: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    pub connect_timeout: Duration,
}

impl TransportConfig {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            refresh_token: None,
            ack_timeout: DEFAULT_ACK_TIMEOUT,
            backoff_base: DEFAULT_BACKOFF_BASE,
            backoff_max: DEFAULT_BACKOFF_MAX,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
        }
    }
}

/// Статус соединения (индикатор в UI, ТЗ §6.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportStatus {
    Connecting,
    Online,
    Reconnecting {
        attempt: u32,
        delay: Duration,
    },
    /// access/refresh истёк → экран входа (§4.5); авто-reconnect остановлен.
    AuthExpired,
    /// Транспорт остановлен (close/падение потока).
    Stopped,
}

/// Причина провала команды без ответа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailReason {
    /// Соединение оборвалось до ack — pending откачен (§2.4).
    Disconnected,
}

/// События от транспорта приложению.
#[derive(Debug)]
pub enum IngressEvent {
    Status(TransportStatus),
    /// Актуализированный ack (в т.ч. ошибки — приложение показывает `message`).
    Ack(Ack),
    /// Серверное событие `evt/*` — дальше типизируется приложением.
    Event(Envelope),
    /// Ack не пришёл за `ack_timeout` и повторы исчерпаны.
    CommandTimeout {
        seq: u32,
        msg_type: String,
    },
    /// Команда потеряна при обрыве соединения.
    CommandFailed {
        seq: u32,
        msg_type: String,
        reason: FailReason,
    },
}

#[derive(Debug)]
pub(crate) enum Outbound {
    Envelope {
        envelope: Envelope,
        idempotent: bool,
    },
    /// Обновить refresh-токен (ротация) — применяется и в backoff-фазе.
    UpdateToken(Option<String>),
    Close,
}

/// Хэндл для отправки команд из любого потока (Clone).
#[derive(Clone)]
pub struct TransportHandle {
    outbound: mpsc::UnboundedSender<Outbound>,
    seq: Arc<AtomicU32>,
}

impl TransportHandle {
    /// Отправить команду: получает `seq` и попадает в очередь ожидания ack'а.
    /// Возвращает `seq` для сопоставления с [`IngressEvent::Ack`].
    pub fn send<C: Command>(&self, cmd: &C) -> Result<u32, TransportError> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let envelope = Envelope::command(seq, now_rfc3339(), cmd)?;
        let idempotent = C::IDEMPOTENT;
        self.outbound
            .send(Outbound::Envelope {
                envelope,
                idempotent,
            })
            .map_err(|_| TransportError::Stopped)?;
        Ok(seq)
    }

    /// Ротация refresh-токена (например, после `session.refresh`).
    pub fn set_refresh_token(&self, token: Option<String>) -> Result<(), TransportError> {
        self.outbound
            .send(Outbound::UpdateToken(token))
            .map_err(|_| TransportError::Stopped)
    }

    /// Грациозная остановка; закрытие канала Ingress = транспорт мёртв.
    pub fn close(&self) {
        let _ = self.outbound.send(Outbound::Close);
    }
}

/// Запуск транспорта в отдельном потоке (`ld2-transport`) со своим рантаймом.
/// Канал `ingress` закрывается при остановке — приложение считает это концом.
pub fn spawn(
    cfg: TransportConfig,
    ingress: mpsc::UnboundedSender<IngressEvent>,
) -> TransportHandle {
    let seq = Arc::new(AtomicU32::new(initial_seq()));
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
    let handle = TransportHandle {
        outbound: outbound_tx,
        seq: Arc::clone(&seq),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("ток-рантайм транспорта");

    std::thread::Builder::new()
        .name("ld2-transport".into())
        .spawn(move || runtime.block_on(connection::run(cfg, ingress, outbound_rx, seq)))
        .expect("поток транспорта");

    handle
}

/// Старт `seq` — случайный (§4.3).
fn initial_seq() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
        .unwrap_or(0x5EED)
}

/// ISO-8601 UTC для поля `ts` (§1.1: `ts` — ISO, дедлайны — epoch).
pub(crate) fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

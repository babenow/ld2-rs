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
    /// M19 (§15): домен discovery — если задан, адрес берётся из
    /// well-known-документа (кэш — на время работы транспорта);
    /// `None` — прямой режим через `host`.
    pub domain: Option<String>,
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
            domain: None,
            refresh_token: None,
            ack_timeout: DEFAULT_ACK_TIMEOUT,
            backoff_base: DEFAULT_BACKOFF_BASE,
            backoff_max: DEFAULT_BACKOFF_MAX,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
        }
    }

    /// M19 (§15): подключение через discovery-домен (ADR-013).
    pub fn with_domain(mut self, domain: Option<String>) -> Self {
        self.domain = domain.filter(|d| !d.trim().is_empty());
        self
    }
}

/// M19 (§15): код провала discovery — локализуется в store
/// (`Key::Disc*`, §15-ошибки понятны без подключения).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveFailCode {
    Unreachable,
    BadDoc,
    Contract,
    MinClient,
    Insecure,
}

impl From<&crate::discovery::ResolveError> for ResolveFailCode {
    fn from(e: &crate::discovery::ResolveError) -> Self {
        use crate::discovery::ResolveError as E;
        match e {
            E::Unreachable(_) => Self::Unreachable,
            E::BadBody(_) | E::BadDomain(_) => Self::BadDoc,
            E::IncompatibleContract { .. } => Self::Contract,
            E::MinClient { .. } => Self::MinClient,
            E::InsecureAddress(_) => Self::Insecure,
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
    /// M19 (§15): discovery не прошёл — подключения нет (ретраи по
    /// backoff продолжаются, документ может быть исправлен).
    ResolveFailed(ResolveFailCode),
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
    /// RTT последней подтверждённой команды, мс (НФР: RTT-индикатор).
    Rtt(u64),
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

#[cfg(test)]
mod tests {
    use super::ResolveFailCode;
    use crate::discovery::ResolveError;

    /// M19 (§15): каждый отказ резолва маппится в свой код статуса
    /// (локализуется в store ключами `Disc*`).
    #[test]
    fn resolve_errors_map_to_status_codes() {
        let unreachable = ResolveError::Unreachable("таймаут".into());
        assert_eq!(
            ResolveFailCode::from(&unreachable),
            ResolveFailCode::Unreachable
        );

        let bad_body = ResolveError::BadBody("нет address".into());
        assert_eq!(ResolveFailCode::from(&bad_body), ResolveFailCode::BadDoc);

        let bad_domain = ResolveError::BadDomain(" ".into());
        assert_eq!(ResolveFailCode::from(&bad_domain), ResolveFailCode::BadDoc);

        let contract = ResolveError::IncompatibleContract {
            server: 2,
            client: 1,
        };
        assert_eq!(ResolveFailCode::from(&contract), ResolveFailCode::Contract);

        let min_client = ResolveError::MinClient {
            need: "9.0.0".into(),
            have: "0.17.0".into(),
        };
        assert_eq!(
            ResolveFailCode::from(&min_client),
            ResolveFailCode::MinClient
        );

        let insecure = ResolveError::InsecureAddress("ws://evil/ws".into());
        assert_eq!(ResolveFailCode::from(&insecure), ResolveFailCode::Insecure);
    }
}

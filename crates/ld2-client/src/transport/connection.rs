//! Основной цикл транспорта: подключение → (resume → resync) → селект-loop
//! → backoff/reconnect (`docs/protocol.md` §2.4, §3, §4.5; PLAN-M1 T2).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use ld2_protocol::cmd::{AuthResume, StateResync};
use ld2_protocol::{Ack, Envelope, ErrorCode, ACK_TYPE};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use super::queue::PendingQueue;
use super::{now_rfc3339, IngressEvent, Outbound, TransportConfig, TransportStatus};

const TICK: std::time::Duration = std::time::Duration::from_secs(1);

/// Чем закончилось соединение.
pub(super) enum Flow {
    /// Запрошенная остановка (`Close`/закрытый канал).
    Stopped,
    /// `E_AUTH_EXPIRED` — авто-reconnect останавливается (§4.5).
    AuthExpired,
    /// Обрыв — нужен backoff и новая попытка.
    Reconnect,
}

/// Фаза bootstrap-цепочки на свежем соединении.
enum Awaiting {
    Resume(u32),
    Resync(u32),
}

enum ServerMsg {
    Ack(Ack),
    Event(Envelope),
    Unknown(String),
}

pub(super) async fn run(
    mut cfg: TransportConfig,
    ingress: mpsc::UnboundedSender<IngressEvent>,
    mut outbound: mpsc::UnboundedReceiver<Outbound>,
    seq: Arc<AtomicU32>,
) {
    let mut attempt: u32 = 0;
    let mut last_state_version: u64 = 0;
    let mut deferred: Vec<Outbound> = Vec::new();

    'outer: loop {
        if attempt > 0 {
            let delay = super::backoff::delay(cfg.backoff_base, cfg.backoff_max, attempt);
            let _ = ingress.send(IngressEvent::Status(TransportStatus::Reconnecting {
                attempt,
                delay,
            }));
            debug!("reconnect #{attempt} через {delay:?}");

            // Фаза ожидания: переживаем Close/UpdateToken, команды буферизуем.
            let deadline = Instant::now() + delay;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                tokio::select! {
                    _ = tokio::time::sleep(remaining) => break,
                    msg = outbound.recv() => match msg {
                        None | Some(Outbound::Close) => break 'outer,
                        Some(Outbound::UpdateToken(t)) => cfg.refresh_token = t,
                        Some(env @ Outbound::Envelope { .. }) => deferred.push(env),
                    }
                }
            }
        } else {
            let _ = ingress.send(IngressEvent::Status(TransportStatus::Connecting));
        }

        attempt += 1;
        match connect_once(
            &mut cfg,
            &ingress,
            &mut outbound,
            &seq,
            &mut last_state_version,
            &mut deferred,
        )
        .await
        {
            Ok(Flow::Stopped) => break,
            Ok(Flow::AuthExpired) => {
                let _ = ingress.send(IngressEvent::Status(TransportStatus::AuthExpired));
                break;
            }
            Ok(Flow::Reconnect) => debug!("соединение потеряно (попытка {attempt})"),
            Err(e) => warn!("ошибка соединения (попытка {attempt}): {e:#}"),
        }
    }

    let _ = ingress.send(IngressEvent::Status(TransportStatus::Stopped));
}

async fn connect_once(
    cfg: &mut TransportConfig,
    ingress: &mpsc::UnboundedSender<IngressEvent>,
    outbound: &mut mpsc::UnboundedReceiver<Outbound>,
    seq: &Arc<AtomicU32>,
    last_state_version: &mut u64,
    deferred: &mut Vec<Outbound>,
) -> Result<Flow, anyhow::Error> {
    let (ws, _response) = tokio::time::timeout(
        cfg.connect_timeout,
        tokio_tungstenite::connect_async(cfg.host.as_str()),
    )
    .await
    .map_err(|_| anyhow::anyhow!("таймаут подключения к {}", cfg.host))??;

    let (sink, stream) = ws.split();
    let _ = ingress.send(IngressEvent::Status(TransportStatus::Online));
    info!("подключено: {}", cfg.host);

    drive_session(
        cfg,
        ingress,
        outbound,
        seq,
        last_state_version,
        deferred,
        sink,
        stream,
    )
    .await
}

// 8 аргументов: состояние сессии, которое иначе пришлось бы собирать в структуру-мутабель.
#[allow(clippy::too_many_arguments)]
async fn drive_session<S, St>(
    cfg: &mut TransportConfig,
    ingress: &mpsc::UnboundedSender<IngressEvent>,
    outbound: &mut mpsc::UnboundedReceiver<Outbound>,
    seq: &Arc<AtomicU32>,
    last_state_version: &mut u64,
    deferred: &mut Vec<Outbound>,
    mut sink: S,
    mut stream: St,
) -> Result<Flow, anyhow::Error>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
    St: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut pending = PendingQueue::new();
    let mut awaiting: Option<Awaiting> = None;
    let mut tick = tokio::time::interval(TICK);

    // Bootstrap: resume (если есть токен) → затем resync (§4.5, §3).
    if let Some(token) = cfg.refresh_token.clone() {
        let env = make_cmd(
            seq,
            &AuthResume {
                refresh_token: token,
            },
        )?;
        awaiting = Some(Awaiting::Resume(env.seq.expect("seq")));
        send_env(&mut sink, &env).await?;
        pending.insert(env, true);
    }

    // Команды, накопленные в буфере на время backoff.
    for out in deferred.drain(..) {
        if let Outbound::Envelope {
            envelope,
            idempotent,
        } = out
        {
            send_env(&mut sink, &envelope).await?;
            pending.insert(envelope, idempotent);
        }
    }

    let result: Result<Flow, anyhow::Error> = 'session: loop {
        tokio::select! {
            incoming = stream.next() => match incoming {
                None => break Ok(Flow::Reconnect),
                Some(Err(e)) => break Err(e.into()),
                Some(Ok(Message::Close(_))) => break Ok(Flow::Reconnect),
                Some(Ok(Message::Text(text))) => {
                    match classify(text.as_str()) {
                        Ok(ServerMsg::Ack(ack)) => {
                            let auth_expired = ack.error_code() == Some(ErrorCode::AuthExpired);

                            // Сначала применяем state_version этого ack'а —
                            // bootstrap `state.resync` обязан нести последнюю
                            // известную версию (§3), а не нулевую.
                            if let Some(v) = ack.state_version {
                                *last_state_version = v;
                            }

                            // Цепочка resume → resync на том же соединении.
                            match awaiting {
                                Some(Awaiting::Resume(s)) if ack.seq == s => {
                                    if ack.is_ok() {
                                        let env = make_cmd(seq, &StateResync {
                                            last_state_version: *last_state_version,
                                        })?;
                                        awaiting = Some(Awaiting::Resync(env.seq.expect("seq")));
                                        send_env(&mut sink, &env).await?;
                                        pending.insert(env, true);
                                    } else {
                                        warn!("auth.resume отклонён: {:?}", ack.error);
                                        awaiting = None;
                                    }
                                }
                                Some(Awaiting::Resync(s)) if ack.seq == s => {
                                    awaiting = None;
                                }
                                _ => {}
                            }

                            if pending.resolve(ack.seq).is_none() {
                                debug!("ack без pending: seq={}", ack.seq);
                            }
                            let _ = ingress.send(IngressEvent::Ack(ack));

                            if auth_expired {
                                break Ok(Flow::AuthExpired);
                            }
                        }
                        Ok(ServerMsg::Event(env)) => {
                            // §2.3: state_version монотонен; устаревшие не двигают отметку.
                            if let Some(v) = env.state_version {
                                *last_state_version = (*last_state_version).max(v);
                            }
                            let _ = ingress.send(IngressEvent::Event(env));
                        }
                        Ok(ServerMsg::Unknown(t)) => {
                            // §2.3: неизвестный тип — игнор с логом, без падений.
                            warn!("неизвестный тип сообщения: {t} (игнор)");
                        }
                        Err(e) => warn!("неразборчивое сообщение: {e}"),
                    }
                }
                Some(Ok(Message::Ping(p))) => {
                    if let Err(e) = sink.send(Message::Pong(p)).await {
                        break Err(anyhow::anyhow!("pong: {e}"));
                    }
                }
                Some(Ok(_)) => {} // бинарные фреймы — вне текстового протокола (§1)
            },

            out = outbound.recv() => match out {
                None | Some(Outbound::Close) => {
                    let _ = sink.close().await;
                    break Ok(Flow::Stopped);
                }
                Some(Outbound::UpdateToken(t)) => cfg.refresh_token = t,
                Some(Outbound::Envelope { envelope, idempotent }) => {
                    pending.insert(envelope.clone(), idempotent);
                    if let Err(e) = send_env(&mut sink, &envelope).await {
                        break Err(anyhow::anyhow!("отправка {}: {e}", envelope.msg_type));
                    }
                }
            },

            _ = tick.tick() => {
                let timeouts = pending.time_outs(cfg.ack_timeout, Instant::now());
                for env in timeouts.retry {
                    debug!("ретрай идемпотентной {} seq={:?}", env.msg_type, env.seq);
                    if let Err(e) = send_env(&mut sink, &env).await {
                        break 'session Err(anyhow::anyhow!("ретрай: {e}"));
                    }
                }
                for (s, msg_type) in timeouts.expired {
                    warn!("таймаут ack: {msg_type} seq={s}");
                    let _ = ingress.send(IngressEvent::CommandTimeout { seq: s, msg_type });
                }
            }
        }
    };

    // Откат неотвеченных: явное поражение вместо тихой потери (§2.4).
    for p in pending.drain() {
        let _ = ingress.send(IngressEvent::CommandFailed {
            seq: p.envelope.seq.unwrap_or_default(),
            msg_type: p.envelope.msg_type,
            reason: super::FailReason::Disconnected,
        });
    }

    result
}

fn make_cmd<C: ld2_protocol::Command>(
    seq: &Arc<AtomicU32>,
    cmd: &C,
) -> Result<Envelope, ld2_protocol::ProtocolError> {
    Envelope::command(seq.fetch_add(1, Ordering::Relaxed), now_rfc3339(), cmd)
}

async fn send_env<S>(sink: &mut S, env: &Envelope) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let json = env.to_json_limited()?;
    sink.send(Message::Text(json.into()))
        .await
        .map_err(|e| anyhow::anyhow!("отправка в ws: {e}"))?;
    Ok(())
}

/// Классификация входящего текста (§2.2, §2.3).
fn classify(text: &str) -> Result<ServerMsg, serde_json::Error> {
    let env: Envelope = serde_json::from_str(text)?;
    if env.msg_type == ACK_TYPE {
        let ack: Ack = serde_json::from_str(text)?;
        Ok(ServerMsg::Ack(ack))
    } else if env.msg_type.starts_with("evt/") {
        Ok(ServerMsg::Event(env))
    } else {
        Ok(ServerMsg::Unknown(env.msg_type))
    }
}

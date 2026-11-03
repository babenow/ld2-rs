//! WS-сервер мока: accept-loop + обработка соединения через [`Session`].

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use ld2_protocol::Envelope;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::chaos::Chaos;
use crate::knobs::Knobs;
use crate::session::{Out, Session};

#[derive(Clone)]
pub struct MockOptions {
    /// Например `127.0.0.1:24817`.
    pub bind: String,
    pub chaos: Chaos,
    /// Подпись в логе/помощи.
    pub scenario: String,
    /// Лимит слотов персонажей в `evt/session.start` (D6, DoD M2).
    pub char_slot_limit: u32,
}

/// Основной цикл: слушаем `opts.bind` вечно.
pub async fn serve(opts: MockOptions) -> anyhow::Result<()> {
    let listener = TcpListener::bind(&opts.bind).await?;
    let addr = listener.local_addr()?;
    info!(
        "мок-сервер слушает ws://{addr} (сценарий: {}, chaos: {}, слотов: {})",
        opts.scenario,
        if opts.scenario == "chaos" {
            "вкл"
        } else {
            "выкл"
        },
        opts.char_slot_limit
    );
    serve_configured(listener, opts.chaos, Knobs::active(), opts.char_slot_limit).await
}

/// Вариант для контрактных тестов: свой listener (эфемерный порт), свой chaos.
pub async fn serve_with_listener(listener: TcpListener, chaos: Chaos) -> anyhow::Result<()> {
    serve_with_knobs(listener, chaos, Knobs::active()).await
}

/// Вариант для сквозных e2e (PLAN-M1 T4): доступ к «ручкам» — запись
/// полученных конвертов, управляемые обрывы.
///
/// `Session` **общая для всех соединений** (как у реального сервера):
/// состояние персонажа и монотонный `state_version` переживают reconnect —
/// без этого `state.resync` после обрыва бессмыслен (§3).
pub async fn serve_with_knobs(
    listener: TcpListener,
    chaos: Chaos,
    knobs: Arc<Knobs>,
) -> anyhow::Result<()> {
    serve_configured(listener, chaos, knobs, crate::fixtures::CHAR_SLOT_LIMIT).await
}

/// Полная конфигурация: лимит слотов задаётся здесь (DoD M2 — конфиг, а не код).
///
/// `Session` **общая для всех соединений** (как у реального сервера):
/// состояние персонажа и монотонный `state_version` переживают reconnect —
/// без этого `state.resync` после обрыва бессмыслен (§3).
pub async fn serve_configured(
    listener: TcpListener,
    chaos: Chaos,
    knobs: Arc<Knobs>,
    char_slot_limit: u32,
) -> anyhow::Result<()> {
    let session = Arc::new(std::sync::Mutex::new(Session::with_char_slot_limit(
        char_slot_limit,
    )));
    loop {
        let (stream, peer) = listener.accept().await?;
        let chaos = chaos.clone();
        let knobs = Arc::clone(&knobs);
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            if let Err(e) = handle(stream, peer, chaos, knobs, session).await {
                warn!("соединение {peer} завершено: {e:#}");
            }
        });
    }
}

async fn handle(
    stream: TcpStream,
    peer: SocketAddr,
    mut chaos: Chaos,
    knobs: Arc<Knobs>,
    session: Arc<std::sync::Mutex<Session>>,
) -> anyhow::Result<()> {
    let index = knobs.connections.fetch_add(1, Ordering::SeqCst) + 1;
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let (mut sink, mut stream) = ws.split();
    debug_conn(peer);

    let mut acks_here = 0usize;
    while let Some(msg) = stream.next().await {
        match msg? {
            Message::Text(text) => {
                // Запись для e2e-ассертов (до обработки).
                if let Ok(env) = serde_json::from_str::<Envelope>(text.as_str()) {
                    knobs.received.lock().expect("mutex").push(env);
                }
                if !knobs.respond.load(Ordering::SeqCst) {
                    continue; // молчим — ждём клиентского таймаута
                }

                // Лок поднимается только на чистой синхронной обработке —
                // IO (sink.send) идёт уже без удержания замка.
                let outs = {
                    let mut session = session.lock().expect("mutex сессии");
                    session.on_text(text.as_str())
                };
                for out in outs {
                    match out {
                        Out::Text(t) => {
                            sink.send(Message::Text(t.into())).await?;
                        }
                        Out::Close => {
                            sink.close().await?;
                            return Ok(());
                        }
                    }
                }
                acks_here += 1;

                // §5 ТЗ: сценарий chaos — детерминированные случайные обрывы;
                // плюс управляемый обрыв соединения №1 (e2e T4).
                if chaos.should_drop() {
                    warn!("chaos: обрыв соединения {peer} (seed-последовательность)");
                    sink.send(Message::Close(None)).await?;
                    return Ok(());
                }
                let drop_after = knobs.drop_first_conn_after.load(Ordering::SeqCst);
                if index == 1 && drop_after > 0 && acks_here >= drop_after {
                    warn!("e2e: управляемый обрыв {peer} после {acks_here} сообщений");
                    sink.send(Message::Close(None)).await?;
                    return Ok(());
                }
            }
            Message::Ping(p) => sink.send(Message::Pong(p)).await?,
            Message::Close(_) => break,
            Message::Binary(_) => {
                // protocol.md §1: бинарные фреймы = ошибка протокола.
                warn!("бинарный фрейм от {peer} — закрываю");
                sink.close().await?;
                break;
            }
            _ => {}
        }
    }
    Ok(())
}

fn debug_conn(peer: SocketAddr) {
    info!("подключение: {peer}");
}

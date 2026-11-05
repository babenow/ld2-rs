//! WS-транспорт §1: апгрейд с subprotocol `ld2.v1`, лимит фрейма 64 KiB,
//! бинарные кадры = ошибка протокола; цикл соединения → [`Session`].

use std::net::SocketAddr;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::ConnectInfo;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use ld2_protocol::envelope::{Envelope, MAX_MESSAGE_BYTES};
use serde_json::json;
use tracing::{info, warn};

use crate::session::{Out, Session};

/// `GET /ws`: подключение (§1). Subprotocol `ld2.v1` — рекомендация §1:
/// если клиент его запросил — отвечаем им, иначе принимаем без него.
pub async fn ws_handler(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade
        .protocols(["ld2.v1"])
        .on_upgrade(move |socket| run_connection(socket, peer))
}

async fn run_connection(socket: WebSocket, peer: SocketAddr) {
    info!("ws: подключился {peer}");
    let (mut sink, mut reader) = socket.split();
    let mut session = Session::new();
    loop {
        let msg = match reader.next().await {
            Some(Ok(msg)) => msg,
            Some(Err(e)) => {
                warn!("ws {peer}: ошибка чтения: {e}");
                break;
            }
            None => break,
        };
        match msg {
            Message::Text(text) => {
                if text.len() > MAX_MESSAGE_BYTES {
                    // §1: >64 KiB → видимая ошибка и закрытие (паритет с моком).
                    warn!("ws {peer}: фрейм {} KiB > 64 KiB", text.len() / 1024);
                    let notice = Envelope::new(
                        "evt/error",
                        Some(json!({
                            "code": "E_TOO_LARGE",
                            "message": "фрейм больше 64 KiB",
                            "fatal": true
                        })),
                    );
                    if let Ok(t) = notice.to_json_limited() {
                        if sink.send(Message::Text(t.into())).await.is_err() {
                            break;
                        }
                    }
                    let _ = sink
                        .send(Message::Close(Some(CloseFrame {
                            code: 1009, // Message Too Big
                            reason: "E_TOO_LARGE".into(),
                        })))
                        .await;
                    break;
                }
                let mut closed = false;
                let mut failed = false;
                for out in session.on_text(&text).await {
                    match out {
                        Out::Text(t) => {
                            if sink.send(Message::Text(t.into())).await.is_err() {
                                failed = true;
                                break;
                            }
                        }
                        Out::Close => {
                            let _ = sink.close().await;
                            closed = true;
                            break;
                        }
                    }
                }
                if closed {
                    return;
                }
                if failed {
                    break;
                }
            }
            Message::Ping(p) => {
                if sink.send(Message::Pong(p)).await.is_err() {
                    break;
                }
            }
            Message::Pong(_) => {}
            Message::Close(_) => break,
            Message::Binary(_) => {
                // §1: бинарные фреймы = ошибка протокола → закрытие.
                warn!("ws {peer}: бинарный фрейм — закрываю");
                let _ = sink.close().await;
                break;
            }
        }
    }
    info!("ws: соединение {peer} закрыто");
}

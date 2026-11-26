//! WS-транспорт §1: апгрейд с subprotocol `ld2.v1`, лимит фрейма 64 KiB,
//! бинарные кадры = ошибка протокола; цикл соединения → [`Session`].

use std::net::SocketAddr;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use ld2_protocol::envelope::{Envelope, MAX_MESSAGE_BYTES};
use serde_json::json;
use tracing::{info, warn};

use crate::session::{Out, Session};
use crate::state::SharedState;

/// `GET /ws`: подключение (§1). Subprotocol `ld2.v1` — рекомендация §1:
/// если клиент его запросил — отвечаем им, иначе принимаем без него.
pub async fn ws_handler(
    State(state): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade
        .protocols(["ld2.v1"])
        .on_upgrade(move |socket| run_connection(socket, peer, state))
}

async fn run_connection(socket: WebSocket, peer: SocketAddr, state: SharedState) {
    info!("ws: подключился {peer}");
    let (mut sink, mut reader) = socket.split();
    // Канал исходящих: тик боя шлёт события в соединение (M12.5, §2.4).
    // R15: bounded — медленный клиент при тик-флуде больше не копит
    // память без границ (переполнение: world::send_to_char снимает
    // рассылку, команды через sink работают; resync восстановит).
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(256);
    let mut session = Session::new(state.clone(), peer, out_tx);

    'conn: loop {
        tokio::select! {
            incoming = reader.next() => {
                let msg = match incoming {
                    Some(Ok(msg)) => msg,
                    Some(Err(e)) => {
                        warn!("ws {peer}: ошибка чтения: {e}");
                        break 'conn;
                    }
                    None => break 'conn,
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
                                    break 'conn;
                                }
                            }
                            let _ = sink
                                .send(Message::Close(Some(CloseFrame {
                                    code: 1009, // Message Too Big
                                    reason: "E_TOO_LARGE".into(),
                                })))
                                .await;
                            break 'conn;
                        }
                        for out in session.on_text(&text).await {
                            match out {
                                Out::Text(t) => {
                                    if sink.send(Message::Text(t.into())).await.is_err() {
                                        break 'conn;
                                    }
                                }
                                Out::Close => {
                                    let _ = sink.close().await;
                                    break 'conn;
                                }
                            }
                        }
                    }
                    Message::Ping(p) => {
                        if sink.send(Message::Pong(p)).await.is_err() {
                            break 'conn;
                        }
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => break 'conn,
                    Message::Binary(_) => {
                        // §1: бинарные фреймы = ошибка протокола → закрытие.
                        warn!("ws {peer}: бинарный фрейм — закрываю");
                        let _ = sink.close().await;
                        break 'conn;
                    }
                }
            }
            // События от тика боя/мира (адресованы персонажу этой сессии).
            outgoing = out_rx.recv() => {
                match outgoing {
                    Some(text) => {
                        if !text.is_empty()
                            && sink.send(Message::Text(text.into())).await.is_err()
                        {
                            break 'conn;
                        }
                    }
                    None => break 'conn,
                }
            }
        }
    }

    // M34: отвязка персонажа — общий helper (бой, рассылка, соц-очистка,
    // события партнёрам); используется и при вытеснении аккаунта.
    if let Some(char_id) = session.selected_char() {
        crate::session::close_char_session(&state, &char_id, &peer.to_string()).await;
    }
    // M34: снять регистрацию аккаунт-сессии (только свою).
    {
        let owner = peer.to_string();
        let mut sessions = state.account_sessions.lock().expect("сессии аккаунтов");
        sessions.retain(|_, (o, _)| o != &owner);
    }
    info!("ws: соединение {peer} закрыто");
}

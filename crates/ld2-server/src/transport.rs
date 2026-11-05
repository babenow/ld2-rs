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
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
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

    // Отвязка персонажа: бой и рассылка (M12.5) + соц-чистка (M12.7):
    // приглашения снимаются, активная сделка отменяется с событием
    // `evt/trade/closed` партнёру.
    if let Some(char_id) = session.selected_char() {
        {
            let mut world = state.world.lock().expect("мир");
            world.unbind_char(&char_id);
        }
        let closed = {
            let mut social = state.social.lock().expect("соц");
            social.drop_invites_for(&char_id);
            social.drop_guild_invites_for(&char_id);
            social.drop_trades_for(&char_id)
        };
        if let Some((participants, offer_id)) = closed {
            let text = crate::session::event_text(
                "evt/trade/closed",
                None,
                serde_json::json!({ "offer_id": offer_id, "reason": "cancelled" }),
            );
            if !text.is_empty() {
                let world = state.world.lock().expect("мир");
                for addr in participants {
                    if addr != char_id {
                        world.send_to_char(&addr, text.clone());
                    }
                }
            }
        }
    }
    info!("ws: соединение {peer} закрыто");
}

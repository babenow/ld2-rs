//! Тестовый ws-сервер для интеграционных тестов транспорта (PLAN-M1 T2/T4).
//!
//! Детерминированный: поведение управляется [`Knobs`]; в проде используется
//! полноценный мок (`ld2-mock-server`, задача T3).

#![allow(dead_code)] // общие хелперы для нескольких тест-бинарей

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use ld2_protocol::Envelope;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

/// Управляемые «ручки» тестового сервера.
pub struct Knobs {
    /// Принятых соединений (нумерация с 1).
    pub connections: AtomicUsize,
    /// Все полученные конверты (в порядке прихода, сквозь соединения).
    pub received: Mutex<Vec<Envelope>>,
    /// Отвечать ли ack'ами (false = молчим → сценарий таймаута).
    pub respond: AtomicBool,
    /// Закрыть соединение №1 после N ack'ов (0 = никогда) → сценарий обрыва.
    pub drop_first_conn_after: AtomicUsize,
    /// Прислать `evt/pong` сразу после подключения.
    pub send_pong_on_connect: AtomicBool,
}

impl Default for Knobs {
    fn default() -> Self {
        Self {
            connections: AtomicUsize::new(0),
            received: Mutex::new(Vec::new()),
            respond: AtomicBool::new(true),
            drop_first_conn_after: AtomicUsize::new(0),
            send_pong_on_connect: AtomicBool::new(false),
        }
    }
}

impl Knobs {
    pub fn received_count(&self, msg_type: &str) -> usize {
        self.received
            .lock()
            .expect("mutex")
            .iter()
            .filter(|e| e.msg_type == msg_type)
            .count()
    }

    pub fn find(&self, msg_type: &str) -> Option<Envelope> {
        self.received
            .lock()
            .expect("mutex")
            .iter()
            .find(|e| e.msg_type == msg_type)
            .cloned()
    }
}

/// Поднять сервер на эфемерном порту; возвращает ws-url и «ручки».
pub async fn spawn_mock() -> (String, Arc<Knobs>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let knobs = Arc::new(Knobs::default());

    let k = Arc::clone(&knobs);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let k = Arc::clone(&k);
            tokio::spawn(serve(stream, k));
        }
    });

    (format!("ws://{addr}"), knobs)
}

async fn serve(stream: TcpStream, knobs: Arc<Knobs>) {
    let index = knobs.connections.fetch_add(1, Ordering::SeqCst) + 1;
    let ws = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(_) => return,
    };
    let (mut sink, mut stream) = ws.split();

    if knobs.send_pong_on_connect.load(Ordering::SeqCst) {
        let pong = r#"{"v":1,"type":"evt/pong","ts":"2026-11-03T00:00:00.000Z",
                       "payload":{"ts":"2026-11-03T00:00:00.000Z","rtt_ms":1}}"#;
        let _ = sink.send(Message::Text(pong.into())).await;
    }

    let mut acks_here = 0usize;
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => {
                let Ok(env) = serde_json::from_str::<Envelope>(text.as_str()) else {
                    continue;
                };
                knobs.received.lock().expect("mutex").push(env.clone());

                if !knobs.respond.load(Ordering::SeqCst) {
                    continue; // молчим — ждём клиентского таймаута
                }

                let ack = serde_json::json!({
                    "v": 1, "type": "ack", "seq": env.seq.unwrap_or(0),
                    "state_version": 42, "result": "ok"
                });
                let _ = sink.send(Message::Text(ack.to_string().into())).await;
                acks_here += 1;

                let drop_after = knobs.drop_first_conn_after.load(Ordering::SeqCst);
                if index == 1 && drop_after > 0 && acks_here >= drop_after {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

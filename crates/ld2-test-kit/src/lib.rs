//! Общий тестовый харнесс контракта ld2 (D32, PLAN-M12): WS-клиент и базовые
//! проверки §1/§2/§5, которые гоняются **и на моке, и на ld2-server** —
//! расхождения реализаций ловит один и тот же набор.
//!
//! Dev-only crate: в релизные артефакты и Docker-образ не входит.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// WS-клиент тестов: команды с `seq`, ожидание `ack`/`evt/*` (protocol.md §2).
pub struct WsClient {
    sink: futures_util::stream::SplitSink<WsStream, Message>,
    reader: futures_util::stream::SplitStream<WsStream>,
    seq: u32,
}

impl WsClient {
    /// Подключение к `ws://…/ws` (без subprotocol — сервер принимает и так, §1).
    pub async fn connect(url: &str) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("подключение к серверу");
        let (sink, reader) = ws.split();
        Self {
            sink,
            reader,
            seq: 0,
        }
    }

    /// Отправка команды со следующим `seq`; возвращает отправленный seq.
    pub async fn send(&mut self, msg_type: &str, payload: Value) -> u32 {
        self.seq += 1;
        let seq = self.seq;
        self.send_with_seq(msg_type, payload, seq).await;
        seq
    }

    /// Отправка команды с **конкретным** seq (проверка кэша §2.4.2).
    pub async fn send_with_seq(&mut self, msg_type: &str, payload: Value, seq: u32) {
        let env = json!({
            "v": 1, "type": msg_type, "seq": seq,
            "ts": "2026-01-01T00:00:00Z", "payload": payload
        });
        self.sink
            .send(Message::Text(env.to_string().into()))
            .await
            .expect("отправка");
    }

    /// Текущий `seq` (для повторной отправки того же номера, §2.4.2).
    pub fn current_seq(&self) -> u32 {
        self.seq
    }

    /// Сырой текстовый фрейм (проверка лимита 64 KiB, §1 — мимо `send`).
    pub async fn send_raw(&mut self, text: &str) {
        self.sink
            .send(Message::Text(text.into()))
            .await
            .expect("отправка сырого фрейма");
    }

    /// Аборт соединения с клиента (Close-фрейм) — тесты обрывов.
    pub async fn close(&mut self) {
        self.sink.send(Message::Close(None)).await.expect("close");
    }

    /// Следующий текстовый JSON; `{"type":"__close__"}` — закрытие потока.
    pub async fn recv_json(&mut self) -> Value {
        loop {
            let msg = self
                .reader
                .next()
                .await
                .expect("поток закрыт раньше события");
            match msg.expect("ошибка ws") {
                Message::Text(t) => return serde_json::from_str(t.as_str()).expect("json"),
                Message::Close(_) => return json!({ "type": "__close__" }),
                _ => continue,
            }
        }
    }

    /// Приём до первого сообщения, удовлетворяющего предикату (таймаут 5 с).
    pub async fn wait<F: FnMut(&Value) -> bool>(&mut self, mut pred: F) -> Value {
        let fut = async {
            loop {
                let env = self.recv_json().await;
                if pred(&env) {
                    return env;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .expect("таймаут ожидания сообщения")
    }

    /// Ack на данный seq (§2.2).
    pub async fn ack(&mut self, seq: u32) -> Value {
        self.wait(|v| v["type"] == "ack" && v["seq"] == seq).await
    }

    /// Событие данного wire-типа (`evt/…`).
    pub async fn wait_event(&mut self, msg_type: &str) -> Value {
        self.wait(|v| v["type"] == msg_type).await
    }

    /// Ожидание Close от сервера.
    pub async fn wait_close(&mut self) {
        let fut = async {
            loop {
                let msg = self.reader.next().await.expect("поток закрыт");
                match msg {
                    Ok(Message::Close(_)) | Err(_) => return,
                    Ok(_) => continue,
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .expect("таймаут ожидания Close");
    }
}

/// Базовый контрактный набор (D32): гоняется и на моке, и на ld2-server.
/// Ожидаемые значения (лимит слотов) — параметр: конфиг у серверов свой.
pub mod checks {
    use super::WsClient;
    use serde_json::{json, Value};

    /// §5: login → ack ok + `evt/session.start` со всеми обязательными
    /// секциями и лимитом слотов из конфига сервера.
    pub async fn hello(
        c: &mut WsClient,
        login: &str,
        password: &str,
        expected_slot_limit: u64,
    ) -> Value {
        let seq = c
            .send(
                "cmd/auth.login",
                json!({ "login": login, "password": password }),
            )
            .await;
        let ack = c.ack(seq).await;
        assert_eq!(ack["result"], "ok", "login: {ack}");
        let start = c.wait_event("evt/session.start").await;
        let p = &start["payload"];
        for key in [
            "server",
            "session",
            "char_slot_limit",
            "features",
            "crime",
            "chat",
            "inventory",
            "upgrade",
            "magic",
            "characters",
        ] {
            assert!(p.get(key).is_some(), "session.start без секции {key}: {p}");
        }
        assert_eq!(
            p["char_slot_limit"], expected_slot_limit,
            "лимит слотов — из конфига сервера (DoD M2)"
        );
        assert!(
            p["session"]["access_token"].is_string(),
            "access_token в §5"
        );
        assert!(
            p["session"]["refresh"]["token"].is_string(),
            "refresh-токен в §5"
        );
        p.clone()
    }

    /// §7: ping → ack + `evt/pong`; time.sync → ack + `evt/time.sync`
    /// с epoch-серверным временем (§1.1).
    pub async fn ping_and_time_sync(c: &mut WsClient) {
        let seq = c.send("cmd/ping", json!({})).await;
        assert_eq!(c.ack(seq).await["result"], "ok");
        let pong = c.wait_event("evt/pong").await;
        assert!(pong["payload"]["ts"].is_string(), "ts в pong: {pong}");
        assert!(
            pong["payload"]["rtt_ms"].is_number(),
            "rtt_ms в pong: {pong}"
        );

        let seq = c.send("cmd/time.sync", json!({})).await;
        assert_eq!(c.ack(seq).await["result"], "ok");
        let sync = c.wait_event("evt/time.sync").await;
        assert!(
            sync["payload"]["server_ts"].is_i64(),
            "epoch-секунды §1.1: {sync}"
        );
    }

    /// §2.4.2: повтор уже обработанного seq → **точный** кэшированный ack,
    /// повторно событие НЕ приходит.
    pub async fn seq_window_deduplicates(c: &mut WsClient) {
        let seq = c.send("cmd/ping", json!({})).await;
        let first = c.ack(seq).await;
        assert_eq!(first["result"], "ok");
        let _pong = c.wait_event("evt/pong").await;

        c.send_with_seq("cmd/ping", json!({}), seq).await;
        let second = c.recv_json().await;
        assert_eq!(
            second["type"], "ack",
            "повтор — ровно один ack, без событий: {second}"
        );
        assert_eq!(second, first, "точный кэшированный ack (§2.4.2)");
    }

    /// §11.5/§13: неизвестный `cmd/*` → ack `E_INTERNAL`, соединение живо.
    pub async fn unknown_command(c: &mut WsClient) {
        let seq = c.send("cmd/nonsense.thing", json!({})).await;
        let ack = c.ack(seq).await;
        assert_eq!(
            ack["error"]["code"], "E_INTERNAL",
            "неизвестная команда: {ack}"
        );
        let seq = c.send("cmd/ping", json!({})).await;
        assert_eq!(
            c.ack(seq).await["result"],
            "ok",
            "сервер пережил неизвестную команду"
        );
        let _ = c.wait_event("evt/pong").await;
    }

    /// §1: фрейм >64 KiB → `evt/error` `E_TOO_LARGE` и закрытие соединения
    /// (последняя проверка на соединении — оно закрывается).
    pub async fn frame_size_limit(c: &mut WsClient) {
        let big = format!(
            r#"{{"v":1,"type":"cmd/ping","seq":1,"ts":"2026-01-01T00:00:00Z","payload":{{"pad":"{}"}}"}}"#,
            "x".repeat(70_000)
        );
        c.send_raw(&big).await;
        let notice = c.recv_json().await;
        assert_eq!(notice["type"], "evt/error", "ошибка до закрытия: {notice}");
        assert_eq!(notice["payload"]["code"], "E_TOO_LARGE", "{notice}");
        c.wait_close().await;
    }

    /// ADR-006: регистрация — успех (вход без персонажей), слабый пароль,
    /// занятый логин. Логин уникален между прогонами (у ld2-server БД
    /// переживает перезапуски).
    pub async fn register_flow(c: &mut WsClient) {
        let login = unique_login();
        let seq = c
            .send(
                "cmd/auth.register",
                json!({ "login": login, "password": "longenough1" }),
            )
            .await;
        let ack = c.ack(seq).await;
        assert_eq!(ack["result"], "ok", "регистрация: {ack}");
        let start = c.wait_event("evt/session.start").await;
        assert!(
            start["payload"]["characters"]
                .as_array()
                .is_some_and(|a| a.is_empty()),
            "новый аккаунт — без персонажей: {start}"
        );

        let seq = c
            .send(
                "cmd/auth.register",
                json!({ "login": "other_name", "password": "short" }),
            )
            .await;
        assert_eq!(c.ack(seq).await["error"]["code"], "E_WEAK_PASSWORD");

        let seq = c
            .send(
                "cmd/auth.register",
                json!({ "login": login, "password": "longenough1" }),
            )
            .await;
        assert_eq!(c.ack(seq).await["error"]["code"], "E_LOGIN_TAKEN");
    }

    /// Уникальный логин для прогонов против персистентной БД.
    fn unique_login() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("время монотонно")
            .as_nanos();
        format!("t{nanos:x}")
    }
}

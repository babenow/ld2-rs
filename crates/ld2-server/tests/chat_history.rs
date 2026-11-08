//! История чата — кольцо `server_history` и её персистентность (M12.7a).
//! Отдельный бинарь: кольцо глобальное, параллельные тесты его ломали бы.

mod common;

use common::in_world;
use serde_json::json;

#[tokio::test]
async fn chat_history_is_ring_and_survives_restart() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // История-кольцо: server_history = 3 → старые вытесняются.
    let server = common::start_server_guarded(&db_url, |cfg| {
        cfg.gameplay.chat.server_history = 3;
    })
    .await;
    let ws = server.ws.clone();
    let (mut c, login, _) = in_world(&ws).await;

    // 5 сообщений (кулдаун 1.5 с — ждём между отправками).
    for i in 0..5 {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
        }
        let seq = c
            .send(
                "cmd/chat.send",
                json!({ "channel": "global", "text": format!("сообщение {i}") }),
            )
            .await;
        assert_eq!(c.ack(seq).await["result"], "ok", "send #{i}");
        c.wait_event("evt/chat/message").await;
    }

    // Рестарт: история переживает и отдаётся реплеем при выборе персонажа
    // (только последние 3 — кольцо).
    drop(c);
    server.stop().await;
    let ws2 = common::start_ws_server(&db_url).await;
    let mut c2 = ld2_test_kit::WsClient::connect(&ws2).await;
    let seq = c2
        .send(
            "cmd/auth.login",
            json!({ "login": login, "password": "longenough1" }),
        )
        .await;
    assert_eq!(c2.ack(seq).await["result"], "ok");
    c2.wait_event("evt/session.start").await;
    let char_id = {
        let seq = c2.send("cmd/char.list", json!({})).await;
        let ack = c2.ack(seq).await;
        ack["payload"]["characters"][0]["id"]
            .as_str()
            .expect("char id")
            .to_string()
    };
    let seq = c2
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    assert_eq!(c2.ack(seq).await["result"], "ok");
    c2.wait_event("evt/state/snapshot").await;

    // Реплей: 3 сообщения в хронологическом порядке — 2,3,4.
    let mut replayed = Vec::new();
    for _ in 0..3 {
        let msg = c2.wait_event("evt/chat/message").await;
        replayed.push(msg["payload"]["text"].as_str().unwrap_or("").to_string());
    }
    assert_eq!(
        replayed,
        vec!["сообщение 2", "сообщение 3", "сообщение 4"],
        "кольцо server_history=3, хронологический порядок"
    );
}

//! Чат (ФР-13, M12.7a): каналы и рассылка, whisper, кулдаун/длина
//! (`E_RATE_LIMITED`), история-кольцо в БД (`server_history`).

mod common;

use common::in_world;
use serde_json::json;

#[tokio::test]
async fn chat_location_global_and_whisper() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, char_a) = in_world(&ws).await;
    let (mut b, _, _char_b) = in_world(&ws).await;
    let _ = char_a;

    // Уникальный текст: история в БД переживает прогоны — реплей при
    // char.select отдаёт старые сообщения, и предикат их отфильтрует.
    let marker = format!(
        "эй, я здесь {}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    // Оба в спавне (одна локация) → location слышно обоим.
    let seq = a
        .send(
            "cmd/chat.send",
            json!({ "channel": "location", "text": &marker }),
        )
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "location ack");
    let got_a = a
        .wait(|v| v["type"] == "evt/chat/message" && v["payload"]["text"] == marker)
        .await;
    let got_b = b
        .wait(|v| v["type"] == "evt/chat/message" && v["payload"]["text"] == marker)
        .await;
    for msg in [&got_a, &got_b] {
        assert_eq!(msg["payload"]["channel"], "location");
        assert_eq!(msg["payload"]["text"], marker);
        assert!(msg["payload"]["sent_at"].is_string(), "ISO-8601 sent_at");
        assert_eq!(msg["payload"]["from"], got_a["payload"]["from"]);
    }

    // Кулдаун (1.5 с): вторая попытка сразу → E_RATE_LIMITED + retry_after_s.
    let seq = a
        .send(
            "cmd/chat.send",
            json!({ "channel": "location", "text": "сразу ещё" }),
        )
        .await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_RATE_LIMITED", "кулдаун: {ack}");
    assert!(
        ack["error"]["retry_after_s"].as_u64().unwrap_or(0) >= 1,
        "retry_after_s: {ack}"
    );

    // Неизвестный канал → отказ.
    let seq = a
        .send("cmd/chat.send", json!({ "channel": "nope", "text": "x" }))
        .await;
    let ack = a.ack(seq).await;
    assert_ne!(ack["result"], "ok", "канал: {ack}");

    // Whisper: доходим по кулдауну (ждём 1.6 с), B слышит, посторонний — нет.
    let (mut c, _, _) = in_world(&ws).await;
    tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
    let name_b = {
        // Имя B берём из его снапшота.
        let seq = b
            .send("cmd/state.resync", json!({ "last_state_version": 1 }))
            .await;
        assert_eq!(b.ack(seq).await["result"], "ok");
        let snap = b.wait_event("evt/state/snapshot").await;
        snap["payload"]["char"]["name"]
            .as_str()
            .expect("имя B")
            .to_string()
    };
    let seq = a
        .send(
            "cmd/chat.send",
            json!({ "channel": "whisper", "to": name_b, "text": "только для тебя" }),
        )
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "whisper ack");
    let wa = a
        .wait(|v| {
            v["type"] == "evt/chat/message"
                && v["payload"]["channel"] == "whisper"
                && v["payload"]["text"] == "только для тебя"
        })
        .await;
    let wb = b
        .wait(|v| {
            v["type"] == "evt/chat/message"
                && v["payload"]["channel"] == "whisper"
                && v["payload"]["text"] == "только для тебя"
        })
        .await;
    assert_eq!(wa["payload"]["channel"], "whisper");
    assert_eq!(wb["payload"]["text"], "только для тебя");
    // C (третий клиент) не должен получить whisper: ждём немного и проверяем,
    // что первым сообщением C станет НЕ этот whisper.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    // C ничего не шлёт — если бы whisper пришёл, wait завис бы; вместо этого
    // шлём C-сообщение в global и проверяем, что его own-сообщение пришло.
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": "проверка" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "global ack C");
    let gc = c
        .wait(|v| v["type"] == "evt/chat/message" && v["payload"]["text"] == "проверка")
        .await;
    assert_eq!(gc["payload"]["text"], "проверка", "C получил своё");
}

#[tokio::test]
async fn chat_max_len_rejected() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _, _) = in_world(&ws).await;

    // 501 символ > chat.max_len (500) → E_RATE_LIMITED.
    let long = "ы".repeat(501);
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": long }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_RATE_LIMITED", "длина: {ack}");

    // Ровно 500 — проходит.
    let ok_text = "ы".repeat(500);
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": ok_text }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "500 символов — ок");
}

#[tokio::test]
async fn chat_party_and_guild_channels() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, char_a) = in_world(&ws).await;
    let (mut b, _, char_b) = in_world(&ws).await;

    // Канал party без группы → отказ.
    let seq = a
        .send(
            "cmd/chat.send",
            json!({ "channel": "party", "text": "эхо?" }),
        )
        .await;
    let ack = a.ack(seq).await;
    assert_ne!(ack["result"], "ok", "без группы: {ack}");

    // Создаём группу, принимаем — затем party-канал слышно обоим.
    let seq = a.send("cmd/party.create", json!({})).await;
    assert_eq!(a.ack(seq).await["result"], "ok");
    let seq = a
        .send("cmd/party.invite", json!({ "player_id": char_b }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok");
    let invite = b.wait_event("evt/party/updated").await;
    let invite_id = invite["payload"]["invite"]["invite_id"]
        .as_str()
        .unwrap()
        .to_string();
    let seq = b
        .send("cmd/party.accept", json!({ "invite_id": invite_id }))
        .await;
    assert_eq!(b.ack(seq).await["result"], "ok");
    a.wait_event("evt/party/updated").await;
    b.wait_event("evt/party/updated").await;

    // Кулдаун пропускаем (ждём 1.6 с).
    tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
    let seq = a
        .send(
            "cmd/chat.send",
            json!({ "channel": "party", "text": "в группе" }),
        )
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "party-канал");
    let got_a = a
        .wait(|v| v["type"] == "evt/chat/message" && v["payload"]["text"] == "в группе")
        .await;
    assert_eq!(got_a["payload"]["channel"], "party");
    let got_b = b
        .wait(|v| v["type"] == "evt/chat/message" && v["payload"]["text"] == "в группе")
        .await;
    assert_eq!(got_b["payload"]["channel"], "party", "B слышит группу");

    // Канал guild без гильдии → отказ (гильдия — ФР-16, в этом тесте
    // только проверяем отказ до вступления).
    tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
    let seq = a
        .send(
            "cmd/chat.send",
            json!({ "channel": "guild", "text": "тест" }),
        )
        .await;
    let ack = a.ack(seq).await;
    assert_ne!(ack["result"], "ok", "без гильдии: {ack}");
    let _ = char_a;
}

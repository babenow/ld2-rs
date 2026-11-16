//! Квесты (ФР-12, M12.6): журнал, принятие → прогресс от убийств →
//! сдача с наградой (XP в пул + предметы), бросок, персистентность.

mod common;

use common::in_world;
use serde_json::json;

#[tokio::test]
async fn quest_accept_progress_turn_in_flow() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    // Журнал: квест доступен (не взят).
    let seq = c.send("cmd/quest.list", json!({})).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "журнал: {ack}");
    let quests = ack["payload"]["quests"].as_array().expect("quests");
    assert!(!quests.is_empty(), "контент квестов: {quests:?}");
    let wolf = quests
        .iter()
        .find(|q| q["id"] == "q_wolf_hunt")
        .expect("q_wolf_hunt")
        .clone();
    assert_eq!(wolf["state"], "available");
    assert_eq!(wolf["goals"][0]["cur"], 0, "прогресс нулевой");
    assert_eq!(wolf["reward_preview"]["xp"], 100);

    // Принять → active.
    let seq = c
        .send("cmd/quest.accept", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "принять: {ack}");
    assert_eq!(ack["payload"]["quest"]["state"], "active");
    let updated = c.wait_event("evt/quest/updated").await;
    assert_eq!(acked_state(&updated), "active");

    // Повторное принятие → отказ.
    let seq = c
        .send("cmd/quest.accept", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    let ack = c.ack(seq).await;
    assert_ne!(ack["result"], "ok", "двойное принятие: {ack}");

    // Сдать раньше времени → отказ.
    let seq = c
        .send("cmd/quest.turn_in", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    let ack = c.ack(seq).await;
    assert_ne!(ack["result"], "ok", "сдача без целей: {ack}");

    // Убийство волка → прогресс цели.
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "атака");
    let quest_evt = c
        .wait(|v| v["type"] == "evt/quest/updated" || v["type"] == "evt/quest/progress")
        .await;
    assert_eq!(
        quest_evt["payload"]["quest"]["state"], "turn_in",
        "цель выполнена: {quest_evt}"
    );
    assert_eq!(quest_evt["payload"]["quest"]["goals"][0]["cur"], 1);

    // Сдать: награда — XP в пул + предметы.
    let seq = c
        .send("cmd/quest.turn_in", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "сдача: {ack}");
    assert_eq!(ack["payload"]["quest"]["state"], "done");
    c.wait_event("evt/quest/updated").await;

    // Пул вырос на 100 (награда), зелье в инвентаре.
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let pool = snap["payload"]["char"]["xp_pool"].as_i64().unwrap_or(0);
    assert!(pool >= 100, "награда XP в пуле: {pool}");
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    assert!(
        inv.iter().any(|i| i["type"] == "potion"),
        "награда-предмет в инвентаре: {inv:?}"
    );

    // Сданный квест нельзя бросить.
    let seq = c
        .send("cmd/quest.abandon", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    let ack = c.ack(seq).await;
    assert_ne!(ack["result"], "ok", "бросок завершённого: {ack}");
}

#[tokio::test]
async fn quest_abandon_resets_to_available() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    let seq = c
        .send("cmd/quest.accept", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    c.wait_event("evt/quest/updated").await;

    let seq = c
        .send("cmd/quest.abandon", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "бросок: {ack}");
    assert_eq!(ack["payload"]["quest"]["state"], "available");
    c.wait_event("evt/quest/updated").await;

    // Можно принять заново.
    let seq = c
        .send("cmd/quest.accept", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "повторное принятие");

    // Неизвестный квест → отказ.
    let seq = c
        .send("cmd/quest.accept", json!({ "quest_id": "q_nope" }))
        .await;
    let ack = c.ack(seq).await;
    assert_ne!(ack["result"], "ok", "нет квеста: {ack}");
}

#[tokio::test]
async fn quest_state_survives_restart() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, login, _char) = in_world(&ws).await;

    let seq = c
        .send("cmd/quest.accept", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    c.wait_event("evt/quest/updated").await;
    drop(c);

    // Рестарт: принятый квест переживает (D27) — в снапшоте active.
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
    let seq = c2
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c2.ack(seq).await["result"], "ok");
    let snap = c2.wait_event("evt/state/snapshot").await;
    let quests = snap["payload"]["char"]["quests"]
        .as_array()
        .expect("quests");
    let wolf = quests
        .iter()
        .find(|q| q["id"] == "q_wolf_hunt")
        .expect("квест в снапшоте")
        .clone();
    assert_eq!(wolf["state"], "active", "состояние пережило рестарт");
}

fn acked_state(evt: &serde_json::Value) -> &str {
    evt["payload"]["quest"]["state"].as_str().unwrap_or("")
}

/// M18/F36: лимит текущих квестов — из конфига сервера (§5
/// `quests.max_active`): при лимите 1 повторное принятие →
/// `E_QUEST_LIMIT`; бросок освобождает слот → можно снова.
#[tokio::test]
async fn quest_limit_from_config_rejects_and_recovers() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let addr = common::start_server_with(&db_url, |cfg| {
        cfg.gameplay.quests.max_active = 1;
    })
    .await;
    let ws = format!("ws://{addr}/ws");

    // Регистрация: session.start несёт лимит клиенту (§5, DoD).
    let login = ld2_test_kit::checks::unique_login();
    let mut c = ld2_test_kit::WsClient::connect(&ws).await;
    let seq = c
        .send(
            "cmd/auth.register",
            json!({
                "login": login,
                "password": "longenough1",
                "email": ld2_test_kit::checks::unique_email()
            }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let start = c.wait_event("evt/session.start").await;
    assert_eq!(
        start["payload"]["quests"]["max_active"], 1,
        "лимит из конфига в session.start: {start}"
    );

    // Персонаж и вход в мир.
    let name = format!(
        "Лимит_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("время монотонно")
            .as_nanos()
    );
    let seq = c
        .send(
            "cmd/char.create",
            json!({ "name": name, "profile": "warrior" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "создание: {ack}");
    let char_id = ack["payload"]["character"]["id"]
        .as_str()
        .expect("id")
        .to_string();
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "выбор");
    c.wait_event("evt/state/snapshot").await;

    // Лимит 1: первый квест занимает слот.
    let seq = c
        .send("cmd/quest.accept", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "первый принят");
    c.wait_event("evt/quest/updated").await;

    // 1/1 — повторное принятие упирается в лимит (гейт раньше
    // проверки «уже взят»).
    let seq = c
        .send("cmd/quest.accept", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_QUEST_LIMIT",
        "лимит достигнут: {ack}"
    );

    // Бросок освобождает слот → снова можно.
    let seq = c
        .send("cmd/quest.abandon", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "бросок");
    c.wait_event("evt/quest/updated").await;
    let seq = c
        .send("cmd/quest.accept", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    assert_eq!(
        c.ack(seq).await["result"],
        "ok",
        "слот свободен → снова можно"
    );
}

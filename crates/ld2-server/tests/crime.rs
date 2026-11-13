//! Преступность (ФР-11, §10, M12.5d): мародёрство с трупа игрока,
//! blend-эскалация срока, журнал `crime.log`, истечение → `evt/crime/removed`,
//! персистентность статусов через рестарт (D27).

mod common;

use common::in_world;
use serde_json::json;

#[tokio::test]
async fn looting_player_corpse_grants_marauder() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    // В лес — к трупу игрока cor_9 (was_player: true).
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "переход");

    // Осмотр: лут трупа игрока.
    let seq = c
        .send("cmd/entity.info", json!({ "entity_id": "cor_9" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "осмотр: {ack}");
    assert_eq!(ack["payload"]["was_player"], true, "труп игрока");
    let loot = ack["payload"]["loot"].as_array().expect("лут").clone();
    assert!(!loot.is_empty(), "в трупе есть лут");

    // Сбор лута → статус «Мародёр» (§10: первый срок — полный, 7200 с).
    let seq = c
        .send(
            "cmd/item.use",
            json!({ "item_uid": loot[0]["uid"].as_str().expect("uid") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "сбор лута");
    c.wait_event("evt/item/added").await;
    let added = c.wait_event("evt/crime/added").await;
    assert_eq!(added["payload"]["status"]["kind"], "marauder", "{added}");
    assert_eq!(
        added["payload"]["log_entry"]["delta_s"], 7200,
        "полный срок"
    );
    assert_eq!(added["payload"]["log_entry"]["total_s"], 7200);
    assert!(added["payload"]["status"]["expires_at"].is_i64());

    // Журнал нарушений (§7).
    let seq = c.send("cmd/crime.log", json!({})).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "журнал: {ack}");
    let entries = ack["payload"]["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1, "одна запись: {entries:?}");
    assert_eq!(entries[0]["kind"], "marauder");
    assert_eq!(entries[0]["delta_s"], 7200);

    // Статус виден в снапшоте (перечитываем лист).
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let statuses = snap["payload"]["char"]["crime"]["statuses"]
        .as_array()
        .expect("statuses");
    assert_eq!(statuses.len(), 1, "статус в снапшоте: {statuses:?}");
    assert_eq!(statuses[0]["kind"], "marauder");
    assert_eq!(statuses[0]["severity"], "medium", "из crime.kinds");
}

#[tokio::test]
async fn second_offense_blends_partial_sum() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    // Первый сбор лута: marauder 7200 (T=0 → полный срок).
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let seq = c
        .send("cmd/entity.info", json!({ "entity_id": "cor_9" }))
        .await;
    let loot = c.ack(seq).await["payload"]["loot"]
        .as_array()
        .expect("лут")
        .clone();
    let seq = c
        .send(
            "cmd/item.use",
            json!({ "item_uid": loot[0]["uid"].as_str().expect("uid") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    c.wait_event("evt/item/added").await;
    let first = c.wait_event("evt/crime/added").await;
    assert_eq!(first["payload"]["log_entry"]["delta_s"], 7200);

    // Второе нарушение того же kind — blend: вклад меньше полного
    // (k=0.5, T=7200): 0.5·7200·(1−7200/604800) = 3514.
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": "/scenario crime marauder" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "сценарий");
    let second = c.wait_event("evt/crime/added").await;
    let delta = second["payload"]["log_entry"]["delta_s"]
        .as_i64()
        .expect("delta");
    assert!(
        delta > 0 && delta < 7200,
        "вклад частичного сложения: {delta}"
    );
    assert_eq!(delta, 3557, "0.5·7200·(1−7200/604800) ≈ 3557");
    // Живой остаток: total_term = expires_at − now (§10) — при wall-clock
    // дрейфе между нарушениями T_cur = 7200 − elapsed, а не константа 7200
    // (в CI elapsed ≥ 1 с → 7199 + delta). Считаем T_cur из `at` журнала —
    // проверка детерминирована, формула из спецификации не размывается.
    let at_first = first["payload"]["log_entry"]["at"]
        .as_i64()
        .expect("at первой записи");
    let at_second = second["payload"]["log_entry"]["at"]
        .as_i64()
        .expect("at второй записи");
    let t_cur = at_first + 7200 - at_second;
    assert!(
        (0..=7200).contains(&t_cur),
        "остаток первого срока в пределах [0; 7200]: {t_cur} \
         (at: {at_first} → {at_second})"
    );
    let total = second["payload"]["log_entry"]["total_s"].as_i64().unwrap();
    assert_eq!(
        total,
        t_cur + delta,
        "T_new = T + delta (T живой = {t_cur})"
    );

    // Журнал: две записи.
    let seq = c.send("cmd/crime.log", json!({ "limit": 10 })).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["payload"]["entries"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn crime_status_expires_with_event() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Срок marauder = 2 с → истечение ловится тик-лупом.
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            if let Some(base) = cfg.gameplay.crime.base.get_mut("marauder_s") {
                *base = 2;
            }
        })
        .await
    );
    let (mut c, _login, _char) = in_world(&ws).await;

    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": "/scenario crime marauder" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let added = c.wait_event("evt/crime/added").await;
    assert_eq!(added["payload"]["log_entry"]["delta_s"], 2, "срок 2 с");

    // Тик-луп снимает статус через ~1–2 с → evt/crime/removed.
    let removed = c.wait_event("evt/crime/removed").await;
    assert_eq!(removed["payload"]["kind"], "marauder", "{removed}");

    // Журнал остаётся после истечения (ФР-11).
    let seq = c.send("cmd/crime.log", json!({})).await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["payload"]["entries"].as_array().unwrap().len(),
        1,
        "журнал пережил истечение"
    );
}

#[tokio::test]
async fn crime_statuses_survive_restart() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, login, _char) = in_world(&ws).await;

    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": "/scenario crime marauder" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    c.wait_event("evt/crime/added").await;
    drop(c);

    // Новый сервер (та же БД): статус и журнал пережили (D27).
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
    let statuses = snap["payload"]["char"]["crime"]["statuses"]
        .as_array()
        .expect("statuses");
    assert_eq!(statuses.len(), 1, "статус пережил рестарт: {statuses:?}");
    assert_eq!(statuses[0]["kind"], "marauder");

    let seq = c2.send("cmd/crime.log", json!({})).await;
    let ack = c2.ack(seq).await;
    assert_eq!(
        ack["payload"]["entries"].as_array().unwrap().len(),
        1,
        "журнал пережил рестарт"
    );
}

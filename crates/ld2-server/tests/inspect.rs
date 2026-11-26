//! M35: ПКМ-осмотр чужого персонажа — уровень, экипировка, профессии.

mod common;

use common::in_world;
use ld2_test_kit::WsClient;
use serde_json::json;

/// Выдать предмет dev-сценарием → uid.
async fn give(c: &mut WsClient, def_id: &str) -> String {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario give {def_id}") }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "give {def_id}: {ack}");
    let added = c.wait_event("evt/item/added").await;
    added["payload"]["item"]["uid"]
        .as_str()
        .expect("uid")
        .to_string()
}

async fn give_gold(c: &mut WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
}

async fn learn(c: &mut WsClient, profession_id: &str) {
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": profession_id }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "learn {profession_id}: {ack}");
    c.wait_event("evt/craft/profession").await;
}

#[tokio::test]
async fn inspect_player_shows_level_equipment_and_professions() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    // Оба персонажа — в стартовой деревне (одна локация).
    let (mut target, _login, target_id) = in_world(&ws).await;
    let (mut viewer, _login2, _viewer_id) = in_world(&ws).await;

    // Цель: профессия + надетое оружие.
    give_gold(&mut target, 1000).await;
    learn(&mut target, "mining").await;
    let dagger = give(&mut target, "dagger_knife").await;
    let seq = target
        .send(
            "cmd/item.equip",
            json!({ "item_uid": dagger, "slot": "main_hand" }),
        )
        .await;
    assert_eq!(target.ack(seq).await["result"], "ok", "экипировка");

    let seq = viewer
        .send("cmd/char.inspect", json!({ "char_id": target_id }))
        .await;
    let ack = viewer.ack(seq).await;
    assert_eq!(ack["result"], "ok", "осмотр: {ack}");
    let ch = &ack["payload"]["char"];
    assert_eq!(ch["id"], target_id);
    assert_eq!(ch["level"], 1, "стартовый уровень");
    assert_eq!(ch["profile"], "warrior");
    let equip = ch["equip"].as_array().expect("equip");
    assert!(
        equip
            .iter()
            .any(|e| e["slot"] == "main_hand" && e["item"]["def_id"] == "dagger_knife"),
        "надетый кинжал виден: {equip:?}"
    );
    let profs = ch["professions"].as_array().expect("professions");
    assert!(
        profs.iter().any(|p| p["id"] == "mining" && p["rank"] == 1),
        "профессия видна: {profs:?}"
    );
}

/// Осмотр недоступен, если персонаж в другой локации.
#[tokio::test]
async fn inspect_requires_same_location() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut target, _login, target_id) = in_world(&ws).await;
    let (mut viewer, _login2, _viewer_id) = in_world(&ws).await;

    // Цель уходит в лес — осмотр из деревни недоступен.
    let seq = target
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(target.ack(seq).await["result"], "ok", "в лес");

    let seq = viewer
        .send("cmd/char.inspect", json!({ "char_id": target_id }))
        .await;
    let ack = viewer.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_TARGET_GONE", "не в локации: {ack}");
}

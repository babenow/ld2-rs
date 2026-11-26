//! R14 (REVIEW-M25): зелье — расход стака **до** лечения (ошибка
//! списания исключает повторное лечение); лечение атомарное
//! относительное `LEAST(hp_max, hp_cur + heal)` — клампится к максимуму.

mod common;

use common::in_world;
use ld2_test_kit::WsClient;
use serde_json::json;

/// Выдать предмет сценарием и вернуть uid (evt/item/added).
async fn give_uid(c: &mut WsClient, def_id: &str) -> String {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario give {def_id}") }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "give {def_id}: {ack}");
    let evt = c.wait_event("evt/item/added").await;
    evt["payload"]["item"]["uid"]
        .as_str()
        .expect("uid предмета")
        .to_string()
}

#[tokio::test]
async fn potion_consumes_first_and_heals_clamped() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _, _) = in_world(&ws).await;

    // Зелье у свежего персонажа (hp полон) — лечение клампится к max.
    let uid = give_uid(&mut c, "potion_heal").await;
    let seq = c.send("cmd/item.use", json!({ "item_uid": uid })).await;
    assert_eq!(c.ack(seq).await["result"], "ok", "зелье применено");

    // R14: расход до лечения — evt/item/removed раньше evt/char/stats.
    let removed = c.wait_event("evt/item/removed").await;
    assert_eq!(removed["payload"]["uid"], uid, "стак удалён: {removed}");
    let stats = c.wait_event("evt/char/stats").await;
    let hp = &stats["payload"]["patch"]["hp"];
    assert_eq!(
        hp["cur"], hp["max"],
        "лечение клампится к максимуму (полное hp): {stats}"
    );

    // Кулдаун: второе применение сразу — предмет уже удалён.
    let seq = c.send("cmd/item.use", json!({ "item_uid": uid })).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "error", "кулер/нет предмета: {ack}");
}

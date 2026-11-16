//! M20/F20c: предметы в локации — контейнер (сундук) с однократным
//! лутом на персонажа и выброс → подбор другим игроком → TTL.

mod common;

use common::in_world;
use serde_json::json;

/// Выдать предмет → uid.
async fn give(c: &mut ld2_test_kit::WsClient, def_id: &str) -> String {
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

#[tokio::test]
async fn container_loot_is_one_time_per_char() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, _) = in_world(&ws).await;
    let (mut b, _, _) = in_world(&ws).await;

    // Оба в Чёрном лесу (сундук в контенте локации).
    for c in [&mut a, &mut b] {
        let seq = c
            .send("cmd/location.move", json!({ "exit_id": "e_north" }))
            .await;
        assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    }

    // Осмотр: лут-список контейнера (паритет трупа).
    let seq = a
        .send(
            "cmd/entity.info",
            json!({ "entity_id": "cont_forest_chest" }),
        )
        .await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "осмотр сундука: {ack}");
    let loot = ack["payload"]["loot"].as_array().expect("лут").clone();
    assert_eq!(loot.len(), 3, "лут сундука: {loot:?}");
    assert_eq!(loot[0]["uid"], "loot:cont_forest_chest:0");

    // Взятие: ack + предмет; повторно — E_ITEM_GONE (однократно).
    let seq = a
        .send(
            "cmd/item.use",
            json!({ "item_uid": "loot:cont_forest_chest:0" }),
        )
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "взять лут");
    a.wait_event("evt/item/added").await;
    let seq = a
        .send(
            "cmd/item.use",
            json!({ "item_uid": "loot:cont_forest_chest:0" }),
        )
        .await;
    assert_eq!(
        a.ack(seq).await["error"]["code"],
        "E_ITEM_GONE",
        "повторно — однократно"
    );
    // У A взятый слот скрыт, у B — доступен.
    let seq = a
        .send(
            "cmd/entity.info",
            json!({ "entity_id": "cont_forest_chest" }),
        )
        .await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["payload"]["loot"].as_array().expect("лут").len(), 2);
    let seq = b
        .send(
            "cmd/entity.info",
            json!({ "entity_id": "cont_forest_chest" }),
        )
        .await;
    let ack = b.ack(seq).await;
    assert_eq!(
        ack["payload"]["loot"].as_array().expect("лут").len(),
        3,
        "у B свой журнал loot_taken"
    );
}

#[tokio::test]
async fn drop_pickup_between_players_and_ttl() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // TTL выброса — 1 с (для теста).
    let server = common::start_server_guarded(&db_url, |cfg| {
        cfg.world.drop_ttl_s = 1;
    })
    .await;
    let ws = server.ws.clone();
    let (mut a, _, _) = in_world(&ws).await;
    let (mut b, _, _) = in_world(&ws).await;

    let uid = give(&mut a, "potion_heal").await;
    let uid2 = give(&mut a, "potion_heal").await;

    // A выбрасывает — всем в локации «{by} выбросил(а)…».
    let seq = a
        .send("cmd/item.drop", json!({ "item_uid": uid, "qty": 1 }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "выброс");
    let dropped_a = a.wait_event("evt/item/dropped").await;
    let dropped_b = b.wait_event("evt/item/dropped").await;
    assert_eq!(dropped_a["payload"]["item_name"], "Зелье лечения");
    assert_eq!(dropped_b["payload"]["qty"], 1);

    // B видит предмет в локации (kind object + extra.ground) и забирает.
    let seq = b
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(b.ack(seq).await["result"], "ok");
    let snap = b.wait_event("evt/state/snapshot").await;
    let ground_id = snap["payload"]["location"]["entities"]
        .as_array()
        .expect("entities")
        .iter()
        .find(|e| e["kind"] == "object" && e["ground"] == true)
        .and_then(|e| e["id"].as_str())
        .expect("предмет на земле")
        .to_string();
    let seq = b
        .send("cmd/item.use", json!({ "item_uid": ground_id }))
        .await;
    assert_eq!(b.ack(seq).await["result"], "ok", "подбор");
    let added = b.wait_event("evt/item/added").await;
    assert_eq!(added["payload"]["item"]["def_id"], "potion_heal");
    // A видит исчезновение объекта из локации.
    let despawn = a.wait_event("evt/entity/despawn").await;
    assert_eq!(despawn["payload"]["entity_id"], ground_id);

    // A больше не владеет выброшенным (uid удалён из инвентаря).
    let seq = a
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok");
    let snap_a = a.wait_event("evt/state/snapshot").await;
    let inv = snap_a["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    assert!(
        !inv.iter().any(|i| i["uid"] == uid),
        "выброшенный ушёл: {inv:?}"
    );

    // TTL: второй выброс исчезает через drop_ttl_s (оба получают
    // «{name} исчез»).
    let seq = a
        .send("cmd/item.drop", json!({ "item_uid": uid2, "qty": 1 }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "второй выброс");
    a.wait_event("evt/item/dropped").await;
    let expired_a = a.wait_event("evt/item/expired").await;
    let expired_b = b.wait_event("evt/item/expired").await;
    assert_eq!(expired_a["payload"]["name"], "Зелье лечения");
    assert_eq!(expired_b["payload"]["name"], "Зелье лечения");
}

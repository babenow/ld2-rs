//! M28/B5: сумки по протоколу (e2e): перенос предмета в сумку и
//! обратно, запрет выброса непустой сумки, снятие сумки освобождает
//! ёмкость (гейт: `LD2_TEST_DATABASE_URL`).

mod common;

use common::in_world;
use serde_json::json;

#[tokio::test]
async fn bags_move_and_capacity_over_ws() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
    let cfg = common::test_config(&db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");

    // Предмет рюкзака — фиксированный слот 5 (как будто лежит в сетке).
    let knife_uid: String = sqlx::query_scalar(
        "INSERT INTO items (character_id, def_id, container, slot_index) \
         VALUES ($1, 'dagger_knife', 'backpack', 5) RETURNING uid",
    )
    .bind(&char_id)
    .fetch_one(&pool)
    .await
    .expect("вставить кинжал");

    // Снапшот: 4 стартовые сумки в контейнере bags.
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    let _ = c.ack(seq).await;
    let snap = c.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    let bags: Vec<&serde_json::Value> = inv.iter().filter(|i| i["container"] == "bags").collect();
    assert_eq!(bags.len(), 4, "стартовые сумки: {inv:?}");
    let bag0 = bags
        .iter()
        .find(|b| b["slot_index"] == 0)
        .expect("сумка в слоте 0");
    let bag_uid = bag0["uid"].as_str().expect("uid сумки").to_string();
    assert_eq!(bag0["bag_slots"], 20, "вместимость сумки в снапшоте");

    // Кинжал лежит в базовом рюкзаке.
    let knife = inv
        .iter()
        .find(|i| i["uid"] == knife_uid)
        .expect("кинжал в снапшоте");
    assert_eq!(knife["container"], "backpack");
    assert_eq!(knife["slot_index"], 5);

    // 1) Кладём кинжал в сумку (to_container = uid сумки, to_index = 0).
    let seq = c
        .send(
            "cmd/item.move",
            json!({
                "item_uid": knife_uid,
                "to_container": bag_uid,
                "to_index": 0,
            }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "перенос в сумку: {ack}");
    assert_eq!(ack["payload"]["container"], bag_uid.as_str());
    assert_eq!(ack["payload"]["slot_index"], 0);

    // 2) Непустую сумку нельзя выбросить.
    let seq = c
        .send("cmd/item.drop", json!({ "item_uid": bag_uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_INVENTORY_FULL",
        "непустая сумка не выбрасывается: {ack}"
    );
    assert_eq!(
        ack["error"]["message"],
        "сумка не пуста — сначала выложите содержимое"
    );

    // 3) Возвращаем кинжал в базовый рюкзак — свободный слот 0.
    let seq = c
        .send(
            "cmd/item.move",
            json!({
                "item_uid": knife_uid,
                "to_container": "backpack",
                "to_index": 0,
            }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "обратно в рюкзак");

    // 4) Теперь пустую сумку можно снять в рюкзак (слот 1).
    let seq = c
        .send(
            "cmd/item.move",
            json!({
                "item_uid": bag_uid,
                "to_container": "backpack",
                "to_index": 1,
            }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "снять сумку");

    // 5) Снапшот: кинжал в рюкзаке, сумка снята и стала обычным
    //    предметом рюкзака; ёмкость уменьшилась.
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    let _ = c.ack(seq).await;
    let snap = c.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    let knife = inv.iter().find(|i| i["uid"] == knife_uid).expect("кинжал");
    assert_eq!(knife["container"], "backpack");
    assert_eq!(knife["slot_index"], 0);
    let bag = inv
        .iter()
        .find(|i| i["uid"] == bag_uid)
        .expect("снятая сумка");
    assert_eq!(bag["container"], "backpack");
    assert_eq!(bag["slot_index"], 1);
    assert_eq!(
        inv.iter().filter(|i| i["container"] == "bags").count(),
        3,
        "одна сумка снята"
    );

    pool.close().await;
}

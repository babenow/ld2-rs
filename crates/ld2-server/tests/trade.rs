//! Сделка (ФР-15, M12.7c): открытие в одной локации, предложение
//! предметов/золота, version-safe блокировка, исполнение (предметы+золото
//! одной транзакцией), отмена.

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

async fn give_gold(c: &mut ld2_test_kit::WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
}

/// Открыть сделку A→B, вернуть offer_id.
async fn open_trade(a: &mut ld2_test_kit::WsClient, player_b: &str) -> String {
    let seq = a
        .send("cmd/trade.open", json!({ "player_id": player_b }))
        .await;
    let ack = a.ack(seq).await;
    assert_eq!(ack["result"], "ok", "открытие: {ack}");
    ack["payload"]["offer_id"]
        .as_str()
        .expect("offer_id")
        .to_string()
}

#[tokio::test]
async fn trade_open_requires_same_location() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, _) = in_world(&ws).await;
    let (mut b, _, char_b) = in_world(&ws).await;

    // Оба в спавне → открытие ок; оба получают state.
    let offer_id = open_trade(&mut a, &char_b).await;
    let sa = a.wait_event("evt/trade/state").await;
    let sb = b.wait_event("evt/trade/state").await;
    assert_eq!(sa["payload"]["offer_id"], offer_id);
    assert_eq!(sb["payload"]["offer_id"], offer_id);
    assert_eq!(sa["payload"]["version"], 1);

    // A уходит в лес → повторное открытие (после cancel) отказ.
    let seq = a
        .send("cmd/trade.cancel", json!({ "offer_id": offer_id }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "отмена");
    a.wait_event("evt/trade/closed").await;
    b.wait_event("evt/trade/closed").await;

    let seq = a
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "в лес");
    let seq = a
        .send("cmd/trade.open", json!({ "player_id": char_b }))
        .await;
    let ack = a.ack(seq).await;
    assert_ne!(ack["result"], "ok", "разные локации: {ack}");
    assert_eq!(ack["error"]["code"], "E_TRADE_PARTNER_GONE");
}

/// F29a: кошелёк персонажа — сумма стаков монет (type=money).
async fn money_of(db_url: &str, char_id: &str) -> i64 {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    let gold: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(i.qty), 0) FROM items i \
         JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 AND d.item_type = 'money'",
    )
    .bind(char_id)
    .fetch_one(&pool)
    .await
    .expect("монеты");
    pool.close().await;
    gold
}

#[tokio::test]
async fn trade_offer_lock_and_execute_transfers_items_and_gold() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, char_a) = in_world(&ws).await;
    let (mut b, _, char_b) = in_world(&ws).await;

    // A даёт нож и 100 золота; B ничего (просто принимает).
    let knife = give(&mut a, "dagger_knife").await;
    give_gold(&mut a, 100).await;

    let offer_id = open_trade(&mut a, &char_b).await;
    a.wait_event("evt/trade/state").await;
    b.wait_event("evt/trade/state").await;

    // A кладёт предмет → version 2, B видит them-сторону.
    let seq = a
        .send(
            "cmd/trade.add_item",
            json!({ "offer_id": offer_id, "item_uid": knife }),
        )
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "add_item");
    let sa = a.wait_event("evt/trade/state").await;
    assert_eq!(sa["payload"]["version"], 2, "version после изменения");
    assert_eq!(
        sa["payload"]["you"].as_array().unwrap().len(),
        1,
        "своя колонка"
    );
    assert!(sa["payload"]["them"].as_array().unwrap().is_empty());
    let sb = b.wait_event("evt/trade/state").await;
    assert_eq!(
        sb["payload"]["them"].as_array().unwrap().len(),
        1,
        "зеркало B"
    );
    assert_eq!(sb["payload"]["them"][0]["uid"], knife);

    // A ставит золото → version 3.
    let seq = a
        .send(
            "cmd/trade.set_gold",
            json!({ "offer_id": offer_id, "gold": 100 }),
        )
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "set_gold");
    a.wait_event("evt/trade/state").await;
    let sb = b.wait_event("evt/trade/state").await;
    assert_eq!(sb["payload"]["them_gold"], 100);
    assert_eq!(sb["payload"]["version"], 3);

    // Устаревшая версия → E_TRADE_STALE (блокировка по version §7).
    let seq = b
        .send(
            "cmd/trade.propose",
            json!({ "offer_id": offer_id, "version": 1 }),
        )
        .await;
    let ack = b.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_TRADE_STALE", "stale: {ack}");

    // B блокирует по актуальной версии (3) — ещё не оба → state.
    let seq = b
        .send(
            "cmd/trade.accept",
            json!({ "offer_id": offer_id, "version": 3 }),
        )
        .await;
    assert_eq!(b.ack(seq).await["result"], "ok", "lock B");
    let sb = b.wait_event("evt/trade/state").await;
    // Зеркало §8: у B `you_locked` — его собственная блокировка (true),
    // `them_locked` — блокировка A (ещё не блокировал → false).
    assert_eq!(sb["payload"]["you_locked"], true, "B locked");
    assert_eq!(sb["payload"]["them_locked"], false, "A ещё не locked");

    // Изменение состава снимает блокировки (§11 trade_version).
    let seq = a
        .send(
            "cmd/trade.set_gold",
            json!({ "offer_id": offer_id, "gold": 50 }),
        )
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok");
    a.wait_event("evt/trade/state").await;
    let sb = b.wait_event("evt/trade/state").await;
    // Изменение состава сняло блокировку B (`you` у B).
    assert_eq!(sb["payload"]["you_locked"], false, "сброс после изменения");
    assert_eq!(sb["payload"]["version"], 4);

    // A блокирует (v4), затем B (v4) → оба → исполнение.
    let seq = a
        .send(
            "cmd/trade.propose",
            json!({ "offer_id": offer_id, "version": 4 }),
        )
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok", "lock A");
    a.wait_event("evt/trade/state").await;
    let seq = b
        .send(
            "cmd/trade.accept",
            json!({ "offer_id": offer_id, "version": 4 }),
        )
        .await;
    let ack_b = b.ack(seq).await;
    assert_eq!(ack_b["result"], "ok", "lock B → done: {ack_b}");

    // Оба получили closed {reason: done}.
    let ca = a.wait_event("evt/trade/closed").await;
    assert_eq!(ca["payload"]["reason"], "done");
    let cb = b.wait_event("evt/trade/closed").await;
    assert_eq!(cb["payload"]["reason"], "done");

    // Итог: нож у B, золото перешло (A: 100−50=50, B: +50).
    let seq = b
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(b.ack(seq).await["result"], "ok");
    let snap = b.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь B");
    assert!(
        inv.iter().any(|i| i["uid"] == knife),
        "нож у B после сделки: {inv:?}"
    );
    let seq = a
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(a.ack(seq).await["result"], "ok");
    let snap = a.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь A");
    assert!(
        !inv.iter().any(|i| i["uid"] == knife),
        "нож ушёл из A: {inv:?}"
    );

    // F29a: монеты перешли стаками той же транзакцией
    // (A: 100−50=50, B: +50).
    assert_eq!(money_of(&db_url, &char_a).await, 50, "кошелёк A");
    assert_eq!(money_of(&db_url, &char_b).await, 50, "кошелёк B");
}

#[tokio::test]
async fn trade_cancel_closes_for_both() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut a, _, _) = in_world(&ws).await;
    let (mut b, _, char_b) = in_world(&ws).await;

    let offer_id = open_trade(&mut a, &char_b).await;
    a.wait_event("evt/trade/state").await;
    b.wait_event("evt/trade/state").await;

    // Отмена партнёром → closed cancelled обоим; повтор — отказ.
    let seq = b
        .send("cmd/trade.cancel", json!({ "offer_id": offer_id }))
        .await;
    assert_eq!(b.ack(seq).await["result"], "ok", "отмена B");
    let cb = b.wait_event("evt/trade/closed").await;
    assert_eq!(cb["payload"]["reason"], "cancelled");
    let ca = a.wait_event("evt/trade/closed").await;
    assert_eq!(ca["payload"]["reason"], "cancelled");

    let seq = a
        .send("cmd/trade.cancel", json!({ "offer_id": offer_id }))
        .await;
    let ack = a.ack(seq).await;
    assert_ne!(ack["result"], "ok", "повторная отмена: {ack}");

    // Сделка закрыта → новая открывается без конфликта.
    let offer_id = open_trade(&mut a, &char_b).await;
    assert!(!offer_id.is_empty());
}

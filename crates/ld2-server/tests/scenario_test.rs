//! M18/F45: `/scenario test` — тестовый рюкзак на 100 позиций (все
//! определения контента по кругу, все раритеты), разные валюты
//! (gold_coin + gem_str) и прогресс: уровень 10 и пул 1000.

mod common;

use common::in_world;
use serde_json::json;

#[tokio::test]
async fn scenario_test_gives_100_tiles_currencies_and_level() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char_id) = in_world(&ws).await;

    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": "/scenario test" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "команда принята");

    // Снапшот после выдачи (100 событий item/added отбрасываются wait'ом).
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "resync");
    let snap = c.wait_event("evt/state/snapshot").await;
    let ch = &snap["payload"]["char"];
    let inv = ch["inventory"].as_array().expect("инвентарь");

    // Ровно 100 позиций (F45): стаки не размножаются, добирка — инстансы.
    assert_eq!(inv.len(), 100, "позиций в рюкзаке: {inv:?}");

    // Все раритеты контента покрыты (F45: common/uncommon/rare/epic).
    for rarity in ["common", "uncommon", "rare", "epic"] {
        assert!(
            inv.iter().any(|i| i["rarity"] == rarity),
            "редкость {rarity} отсутствует: {inv:?}"
        );
    }

    // Разные валюты: золото (≥1000) и валюта магазинов gem_str (≥100).
    let gold: i64 = inv
        .iter()
        .filter(|i| i["def_id"] == "gold_coin")
        .map(|i| i["qty"].as_i64().unwrap_or(0))
        .sum();
    assert!(gold >= 1000, "золото: {gold}");
    let gems: i64 = inv
        .iter()
        .filter(|i| i["def_id"] == "gem_str")
        .map(|i| i["qty"].as_i64().unwrap_or(0))
        .sum();
    assert!(gems >= 100, "валюта gem_str: {gems}");

    // Прогресс: уровень 10 и пул 1000 (F45).
    assert_eq!(ch["level"], 10, "уровень: {ch:?}");
    assert_eq!(ch["xp_pool"], 1000, "пул очков: {ch:?}");
    assert!(
        ch["total_xp"].as_i64().unwrap_or(0) > 0,
        "total_xp положителен: {ch:?}"
    );
}

#[tokio::test]
async fn scenario_test_is_idempotent_enough_to_re_run() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char_id) = in_world(&ws).await;

    // Повторный запуск не ломает счётчик: стаки сливаются, инстансы
    // добавляются — позиций становится больше, но не меньше 100.
    for _ in 0..2 {
        let seq = c
            .send(
                "cmd/chat.send",
                json!({ "channel": "global", "text": "/scenario test" }),
            )
            .await;
        assert_eq!(c.ack(seq).await["result"], "ok", "команда принята");
    }
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "resync");
    let snap = c.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    assert!(
        inv.len() >= 100,
        "после двух запусков позиций не меньше 100: {}",
        inv.len()
    );
}

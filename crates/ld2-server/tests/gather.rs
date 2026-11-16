//! M20/F20b: добыча ресурсов — инструмент, ранг профессии, выдача
//! предметов + XP, исчерпание (жила остаётся, трава исчезает).

mod common;

use common::in_world;
use serde_json::json;

/// Выдать предмет dev-сценарием → uid.
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

/// Начислить золото (обучение у тренера платное).
async fn give_gold(c: &mut ld2_test_kit::WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
}

/// Изучить профессию у тренера (спавн — деревня).
async fn learn(c: &mut ld2_test_kit::WsClient, profession_id: &str) {
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": profession_id }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "learn {profession_id}: {ack}");
    c.wait_event("evt/craft/profession").await;
}

#[tokio::test]
async fn gather_tool_profession_yield_and_exhaustion() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;
    give_gold(&mut c, 1000).await;

    // Ранги: учим шахтёра и травника у Старины (спавн — деревня).
    learn(&mut c, "mining").await;
    learn(&mut c, "herbalist").await;

    // В Чёрный лес (проверки ресурса — по текущей локации).
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");

    // Без инструмента → E_TOOL_MISSING (инструмент раньше ранга).
    let seq = c
        .send(
            "cmd/resource.gather",
            json!({ "entity_id": "r_forest_ore" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_TOOL_MISSING", "нет кирки: {ack}");

    give(&mut c, "pickaxe").await;
    give(&mut c, "sickle").await;

    // Руда ×3: предмет + патч шкалы;4-й — исчерпана (E_TARGET_GONE).
    let seq = c
        .send(
            "cmd/resource.gather",
            json!({ "entity_id": "r_forest_ore" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "первая руда");
    let added = c.wait_event("evt/item/added").await;
    assert_eq!(added["payload"]["item"]["def_id"], "ore_copper");
    // Запас в локации: 3 → 2 (патч entity/move).
    let moved = c.wait_event("evt/entity/move").await;
    assert_eq!(moved["payload"]["entity_id"], "r_forest_ore");
    assert_eq!(moved["payload"]["patch"]["hp"]["cur"], 2);

    for expect_cur in [1, 0] {
        let seq = c
            .send(
                "cmd/resource.gather",
                json!({ "entity_id": "r_forest_ore" }),
            )
            .await;
        assert_eq!(c.ack(seq).await["result"], "ok", "добыча {expect_cur}");
        let moved = c.wait_event("evt/entity/move").await;
        assert_eq!(moved["payload"]["patch"]["hp"]["cur"], expect_cur);
    }
    // Жила остаётся видимой, но пуста → отказ.
    let seq = c
        .send(
            "cmd/resource.gather",
            json!({ "entity_id": "r_forest_ore" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_TARGET_GONE", "исчерпана: {ack}");

    // Трава ×1: исчерпание скрывает объект (despawn_when_empty).
    let seq = c
        .send(
            "cmd/resource.gather",
            json!({ "entity_id": "r_forest_herb" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "трава");
    let added = c.wait_event("evt/item/added").await;
    assert_eq!(added["payload"]["item"]["def_id"], "herb");
    let despawn = c.wait_event("evt/entity/despawn").await;
    assert_eq!(despawn["payload"]["entity_id"], "r_forest_herb");
    let seq = c
        .send(
            "cmd/resource.gather",
            json!({ "entity_id": "r_forest_herb" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_TARGET_GONE",
        "скрыта до респавна: {ack}"
    );

    // Инвентарь: руда стаком ×3, трава ×1; XP шахты — из снапшота
    // (событие evt/craft/profession уже прокручено более поздними
    // wait'ами — как и в боевых тестах).
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    let ore: i64 = inv
        .iter()
        .filter(|i| i["def_id"] == "ore_copper")
        .map(|i| i["qty"].as_i64().unwrap_or(1))
        .sum();
    assert_eq!(ore, 3, "добыто3 руды: {inv:?}");
    let herb: i64 = inv
        .iter()
        .filter(|i| i["def_id"] == "herb")
        .map(|i| i["qty"].as_i64().unwrap_or(1))
        .sum();
    assert_eq!(herb, 1, "собрана1 трава: {inv:?}");

    // XP профессии (паритет крафта): шахта ≥65 за3 добычи.
    let profs = snap["payload"]["char"]["professions"]
        .as_array()
        .expect("professions");
    let mining = profs
        .iter()
        .find(|p| p["id"] == "mining")
        .expect("шахтёр в снапшоте");
    assert!(
        mining["xp"].as_i64().unwrap_or(0) >= 65,
        "XP за добычу: {profs:?}"
    );
}

#[tokio::test]
async fn gather_needs_profession_rank() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;
    give_gold(&mut c, 1000).await;

    // Инструмент есть, профессия не изучена → E_PROFESSION.
    give(&mut c, "pickaxe").await;
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    let seq = c
        .send(
            "cmd/resource.gather",
            json!({ "entity_id": "r_forest_ore" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_PROFESSION",
        "шахтёр не изучен: {ack}"
    );
}

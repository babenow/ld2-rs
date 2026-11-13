//! M18/F47 против реального сервера (гейт: LD2_TEST_DATABASE_URL):
//! бонусы предметов (ADR-010) — снапшот несёт контент-бонусы, надетый
//! предмет применяет атрибут и навык, dev-сценарий добавляет бонус
//! инстансу и он попадает в пересчитанный снапшот.

mod common;

use common::in_world_profile;
use serde_json::json;

/// Выдать предмет dev-сценарием → весь снапшот из `evt/item/added`.
async fn give_item(c: &mut ld2_test_kit::WsClient, def_id: &str) -> serde_json::Value {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario give {def_id}") }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "give {def_id}: {ack}");
    let added = c.wait_event("evt/item/added").await;
    added["payload"]["item"].clone()
}

/// Dev-сценарий (чат) → ack.
async fn scenario(c: &mut ld2_test_kit::WsClient, cmd: &str) -> serde_json::Value {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario {cmd}") }),
        )
        .await;
    c.ack(seq).await
}

/// Свежий снапшот персонажа (char.select → evt/state/snapshot).
async fn snapshot(c: &mut ld2_test_kit::WsClient, char_id: &str) -> serde_json::Value {
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    c.wait_event("evt/state/snapshot").await
}

fn skill_rank(snap: &serde_json::Value, id: &str) -> i64 {
    snap["payload"]["char"]["skills"]
        .as_array()
        .expect("навыки")
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("{id} в снапшоте"))["rank"]
        .as_i64()
        .expect("rank")
}

#[tokio::test]
async fn item_bonuses_snapshot_equip_and_dev_command() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world_profile(&ws, "warrior").await;

    // 1) Снапшот предмета несёт контент-бонусы (drakkar_axe).
    let item = give_item(&mut c, "drakkar_axe").await;
    let uid = item["uid"].as_str().expect("uid").to_string();
    let bonuses = item["bonuses"].as_array().expect("bonuses в снапшоте");
    // 4 бонуса из `bonuses` + 1 из раздела `stats.param` (M18/F47).
    assert_eq!(bonuses.len(), 5, "контент + stats.param: {bonuses:?}");
    assert!(
        bonuses
            .iter()
            .any(|b| b["kind"] == "attr" && b["id"] == "str" && b["value"] == 3),
        "Сила +3: {bonuses:?}"
    );
    assert!(
        bonuses
            .iter()
            .any(|b| b["kind"] == "param" && b["id"] == "accuracy" && b["pct"] == true),
        "Точность +4%: {bonuses:?}"
    );
    assert!(
        bonuses
            .iter()
            .any(|b| b["kind"] == "param" && b["id"] == "crit_chance" && b["value"] == 3),
        "Крит из stats.param: {bonuses:?}"
    );

    // 2) Надетый предмет применяет бонусы: атрибут и ранг навыка.
    let before = snapshot(&mut c, &char_id).await;
    let str_before = before["payload"]["char"]["attrs"]["str"]
        .as_i64()
        .expect("str");
    let rank_before = skill_rank(&before, "sword");
    let seq = c
        .send(
            "cmd/item.equip",
            json!({ "item_uid": uid, "slot": "main_hand" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "экипировка: {ack}");
    let ch = &ack["payload"]["char"];
    assert_eq!(
        ch["attrs"]["str"].as_i64(),
        Some(str_before + 3),
        "бонус атрибута применён: {ch}"
    );
    let sword_rank = ch["skills"]
        .as_array()
        .expect("навыки")
        .iter()
        .find(|s| s["id"] == "sword")
        .expect("sword")["rank"]
        .as_i64()
        .expect("rank");
    assert_eq!(
        sword_rank,
        rank_before + 1,
        "бонус навыка: +1 к рангу: {ch}"
    );

    // 3) Dev-команда: бонус инстансу → патч, после перечитки снапшота
    //    производный параметр вырос (плоский + процент к итогу).
    let before = snapshot(&mut c, &char_id).await;
    let accuracy_before = before["payload"]["char"]["derived"]["accuracy"]
        .as_i64()
        .expect("accuracy");
    let ack = scenario(&mut c, &format!("bonus {uid} param accuracy 10 pct")).await;
    assert_eq!(ack["result"], "ok", "bonus: {ack}");
    let state = c.wait_event("evt/item/state").await;
    assert_eq!(
        state["payload"]["patch"]["bonuses"]
            .as_array()
            .expect("патч bonuses")
            .len(),
        6,
        "бонус dev-команды в эффективном списке (5 контентовых + 1): {state}"
    );
    let after = snapshot(&mut c, &char_id).await;
    let accuracy_after = after["payload"]["char"]["derived"]["accuracy"]
        .as_i64()
        .expect("accuracy");
    assert!(
        accuracy_after > accuracy_before,
        "точность выросла: {accuracy_before} → {accuracy_after}"
    );
}

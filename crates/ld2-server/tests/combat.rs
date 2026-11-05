//! Бой (ФР-05, M12.5) против реального сервера: атаки тик-лупа, смерть
//! моба → despawn → труп с лутом → XP убийства (skills.md §2.3), выход
//! из боя.

mod common;

use common::in_world;
use serde_json::json;

#[tokio::test]
async fn combat_kill_mob_yields_corpse_and_xp() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char_id) = in_world(&ws).await;

    // В лес — к волку (агрессивный, hp 18, урон сервера 6 → 3 удара).
    let ack = {
        let seq = c
            .send("cmd/location.move", json!({ "exit_id": "e_north" }))
            .await;
        c.ack(seq).await
    };
    assert_eq!(ack["result"], "ok", "переход: {ack}");

    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "атака: {ack}");

    // Тик-луп наносит удары: ждём смерть моба (деспан + труп).
    let corpse = c.wait_event("evt/corpse/created").await;
    let corpse_id = corpse["payload"]["entity"]["id"]
        .as_str()
        .expect("id трупа")
        .to_string();
    assert_eq!(corpse["payload"]["entity"]["kind"], "corpse");
    assert!(
        corpse["payload"]["loot_count"].as_u64().unwrap_or(0) >= 1,
        "у волка есть drops: {corpse}"
    );

    // Бой окончен (цель уничтожена) — сервер шлёт end.
    let end = c.wait_event("evt/combat/end").await;
    assert_eq!(end["payload"]["reason"], "цель уничтожена");

    // XP убийства: base 8 + difficulty 3 × per 6 = 26, навык оружия не
    // экипирован → unarmed (skills.md §2.3).
    let xp = c.wait_event("evt/char/xp").await;
    assert_eq!(xp["payload"]["skill_id"], "unarmed");
    assert_eq!(xp["payload"]["amount"], 26, "8 + difficulty(3) × 6");
    assert!(xp["payload"]["xp_to_next"].is_i64());

    // Труп с лутом: осмотр → взятие → в инвентаре (M12.4-механика).
    let seq = c
        .send("cmd/entity.info", json!({ "entity_id": corpse_id }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "осмотр трупа: {ack}");
    let loot = ack["payload"]["loot"].as_array().expect("лут трупа");
    assert_eq!(loot.len(), 1, "drops волка: {loot:?}");
    assert_eq!(loot[0]["name"], "Зелье лечения");
    let seq = c
        .send(
            "cmd/item.use",
            json!({ "item_uid": loot[0]["uid"].as_str().expect("uid") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "взятие лута трупа");
    let added = c.wait_event("evt/item/added").await;
    assert_eq!(added["payload"]["item"]["name"], "Зелье лечения");
}

#[tokio::test]
async fn combat_stop_ends_battle() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char_id) = in_world(&ws).await;

    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");

    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");

    // Выход из боя до смерти цели; повторная атака после убитого моба —
    // цель недоступна (моб уже может быть мёртв — оба исхода валидны).
    let seq = c.send("cmd/combat.stop", json!({})).await;
    assert_eq!(c.ack(seq).await["result"], "ok", "стоп");

    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "no_such_mob" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_TARGET_GONE", "нет цели: {ack}");

    // Побег вне боя → «вы не в бою».
    let seq = c.send("cmd/combat.flee", json!({})).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NOT_IN_LOCATION", "нет боя: {ack}");
}

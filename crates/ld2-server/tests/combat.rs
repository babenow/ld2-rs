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
    let (mut c, _login, char_id) = in_world(&ws).await;

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
    assert_eq!(loot.len(), 2, "drops волка: {loot:?}");
    assert_eq!(loot[0]["name"], "Зелье лечения");
    // F29a: монеты падают с моба обычным предметом (слот 2).
    assert_eq!(loot[1]["name"], "Золотая монета", "монеты лутом");
    let seq = c
        .send(
            "cmd/item.use",
            json!({ "item_uid": loot[0]["uid"].as_str().expect("uid") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "взятие лута трупа");
    let added = c.wait_event("evt/item/added").await;
    assert_eq!(added["payload"]["item"]["name"], "Зелье лечения");
    let seq = c
        .send(
            "cmd/item.use",
            json!({ "item_uid": loot[1]["uid"].as_str().expect("uid") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "взятие монет");
    let added = c.wait_event("evt/item/added").await;
    assert_eq!(added["payload"]["item"]["name"], "Золотая монета");
    assert_eq!(added["payload"]["item"]["qty"], 10, "стак монет из def");

    // M15/F24: общий опыт и шкала уровня в свежем снапшоте (26 XP).
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "перевыбор персонажа");
    let snap = c.wait_event("evt/state/snapshot").await;
    let ch = &snap["payload"]["char"];
    assert!(
        ch["inventory"]
            .as_array()
            .expect("инвентарь")
            .iter()
            .any(|i| i["type"] == "money" && i["qty"] == 10),
        "F29a: монеты в инвентаре: {}",
        ch["inventory"]
    );
    assert_eq!(ch["total_xp"], 26, "total_xp = XP убийства: {ch}");
    assert_eq!(ch["level"], 1, "26 < 200 → уровень 1");
    assert_eq!(ch["xp_in_level"], 26, "прогресс в уровне");
    assert_eq!(ch["xp_to_next"], 100, "порог 1-го уровня: 50+50+0");

    // M15/F26: производные параметры в том же снапшоте.
    let der = ch["derived"].as_object().expect("derived в снапшоте");
    assert_eq!(der["accuracy"], 80, "75 + dex(10)/2");
    assert_eq!(der["damage_min"], 11, "база 6 + str/2");
    assert_eq!(der["armor"], 0, "без брони в стартовой экипировке");
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

/// Ранг навыка напрямую в БД (гейт `power_strike`: владение мечом 5).
async fn set_skill_rank(db_url: &str, char_id: &str, skill: &str, rank: i32) {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    sqlx::query(
        "INSERT INTO character_skills (character_id, skill_id, xp, rank) \
         VALUES ($1, $2, 0, $3) \
         ON CONFLICT (character_id, skill_id) DO UPDATE SET rank = EXCLUDED.rank",
    )
    .bind(char_id)
    .bind(skill)
    .bind(rank)
    .execute(&pool)
    .await
    .expect("ранг");
    pool.close().await;
}

/// `cmd/skill.use` (§4, M14): гейт навыка, урон ×1.8, серверный кулдаун
/// `E_COOLDOWN` + `retry_after_s`, ошибки цели.
#[tokio::test]
async fn skill_use_power_strike_gate_damage_and_cooldown() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;

    // 1) Гейт (§4: открывается владением мечом 5) → E_REQUIREMENT_ATTR;
    //    проверяется раньше цели — моб в другой локации не мешает.
    let seq = c
        .send(
            "cmd/skill.use",
            json!({ "skill_id": "power_strike", "target_id": "m_3" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_REQUIREMENT_ATTR",
        "гейт без ранга: {ack}"
    );

    // 2) Ранг есть → удар проходит: урон 6 × 1.8 = 10.8 → 11 (§4).
    set_skill_rank(&db_url, &char_id, "sword", 5).await;
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    let seq = c
        .send(
            "cmd/skill.use",
            json!({ "skill_id": "power_strike", "target_id": "m_3" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "удар");
    let cd = c.wait_event("evt/combat/cooldown").await;
    assert_eq!(cd["payload"]["skill_id"], "power_strike");
    assert!(
        cd["payload"]["until_epoch"].as_i64().unwrap_or(0) > 0,
        "until_epoch: {cd}"
    );
    let hit = c.wait(|v| v["type"] == "evt/combat/hit").await;
    assert_eq!(hit["payload"]["amount"], 11, "6 × 1.8 = 11: {hit}");
    assert_eq!(hit["payload"]["kind"], "skill");
    assert_eq!(hit["payload"]["target"], "m_3");
    let hp = c
        .wait(|v| v["type"] == "evt/entity/hp" && v["payload"]["entity_id"] == "m_3")
        .await;
    assert_eq!(hp["payload"]["hp"]["cur"], 7, "18 − 11: {hp}");

    // 3) Повтор в кулдауне → E_COOLDOWN + retry_after_s (§7).
    let seq = c
        .send(
            "cmd/skill.use",
            json!({ "skill_id": "power_strike", "target_id": "m_3" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_COOLDOWN",
        "второй удар в кулдауне: {ack}"
    );
    assert!(
        ack["error"]["retry_after_s"].as_u64().unwrap_or(0) >= 1,
        "retry_after_s: {ack}"
    );

    // 4) Цель не указана / нет такой цели → E_TARGET_GONE (гейт и кулдаун
    //    проверяются после цели — запрос валидируется целиком).
    let seq = c
        .send("cmd/skill.use", json!({ "skill_id": "power_strike" }))
        .await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_TARGET_GONE");
    let seq = c
        .send(
            "cmd/skill.use",
            json!({ "skill_id": "power_strike", "target_id": "no_such_mob" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_TARGET_GONE");
}

/// Добивание активной способностью: XP убийства как у обычной атаки (§2.3).
#[tokio::test]
async fn skill_use_power_strike_kills_and_grants_xp() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Короткий кулдаун — второй удар идёт сразу после ожидания перезарядки.
    let addr = common::start_server_with(&db_url, |cfg| {
        cfg.skills
            .active
            .get_mut("power_strike")
            .expect("power_strike в конфиге")
            .cooldown_s = 0.5;
    })
    .await;
    let ws = format!("ws://{addr}/ws");
    let (mut c, _login, char_id) = in_world(&ws).await;
    set_skill_rank(&db_url, &char_id, "sword", 5).await;

    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");

    // Первый удар: 18 − 11 = 7.
    let seq = c
        .send(
            "cmd/skill.use",
            json!({ "skill_id": "power_strike", "target_id": "m_3" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "первый удар");
    c.wait_event("evt/combat/cooldown").await;
    let hit = c.wait(|v| v["type"] == "evt/combat/hit").await;
    assert_eq!(hit["payload"]["amount"], 11, "урон: {hit}");

    // Кулдаун 0.5 с → до 1 с в гранулярности epoch-секунд; ждём с запасом.
    tokio::time::sleep(std::time::Duration::from_millis(1300)).await;

    // Второй удар добивает (11 ≥ 7) → труп + XP убийства (26 = 8 + 3×6).
    let seq = c
        .send(
            "cmd/skill.use",
            json!({ "skill_id": "power_strike", "target_id": "m_3" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "второй удар");
    c.wait_event("evt/combat/cooldown").await;
    let hit = c.wait(|v| v["type"] == "evt/combat/hit").await;
    assert_eq!(hit["payload"]["amount"], 11, "урон добивающего: {hit}");
    let corpse = c.wait_event("evt/corpse/created").await;
    assert_eq!(corpse["payload"]["entity"]["kind"], "corpse");
    let xp = c.wait_event("evt/char/xp").await;
    assert_eq!(xp["payload"]["amount"], 26, "XP убийства: {xp}");
    assert_eq!(
        xp["payload"]["skill_id"], "unarmed",
        "оружие не экипировано"
    );
}

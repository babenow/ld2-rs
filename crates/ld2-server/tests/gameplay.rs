//! M12.4 против реального сервера (гейт: LD2_TEST_DATABASE_URL):
//! навыки/XP-пул (гейты/потолок, skills.md §2.2), инвентарь (лут →
//! экипировка → расход → выброс, ФР-07), смерть с потерей **только лута**
//! (ФР-06) и персистентность прогресса после рестарта (D27).

mod common;

use common::in_world;
use ld2_test_kit::WsClient;
use serde_json::json;

/// `xp_to_next(rank) = round(120 × 1.32^rank)` — как на сервере (§2.2).
fn xp_to_next(rank: i32) -> i64 {
    (120.0 * 1.32f64.powi(rank)).round() as i64
}

/// Начислить золото dev-сценарием (M14: обучение у тренера платное).
async fn give_gold(c: &mut WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "gold {amount}");
}

async fn move_exit(c: &mut WsClient, exit_id: &str) -> serde_json::Value {
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": exit_id }))
        .await;
    c.ack(seq).await
}

#[tokio::test]
async fn skill_train_gates_and_cap() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;

    // Пул наполняем напрямую (квестовые награды — M12.6).
    let cfg = common::test_config(&db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    sqlx::query("UPDATE characters SET xp_pool = 100000 WHERE id = $1")
        .bind(&char_id)
        .execute(&pool)
        .await
        .expect("наполнить пул");

    // M18/F44: NPC не обучает неизвестному навыку (нет записи в диалоге
    // и в маркере локации) → E_NO_TRAINER.
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "no_such" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_NO_TRAINER");

    // Шкала пустая → E_SKILL_NOT_READY (гейт 100 %, ADR-009).
    give_gold(&mut c, 1000).await;
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_SKILL_NOT_READY",
        "шкала не полна: {ack}"
    );

    // Успех: шкала 100 % (SQL), плата из диалога — 10 очков + 100 золота.
    common::set_skill(&pool, &char_id, "sword", xp_to_next(0), 0).await;
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "обучение: {ack}");
    assert_eq!(ack["payload"]["rank"], 1, "ранг +1");
    assert_eq!(ack["payload"]["xp"], 0, "шкала обнулена");
    assert_eq!(ack["payload"]["pool_left"], 99990, "100000 − 10 очков");
    let xp = c.wait_event("evt/char/xp").await;
    assert_eq!(xp["payload"]["skill_id"], "sword");
    assert_eq!(xp["payload"]["rank"], 1);
    assert_eq!(xp["payload"]["amount"], -10, "списаны очки пула");
    let gold =
        ld2_server::db::items::count_by_type(&pool, &char_id, ld2_server::db::items::MONEY_TYPE)
            .await
            .expect("золото");
    assert_eq!(gold, 900, "плата 100 золота списана");

    // Снапшот: навык ранга 1 с пустой шкалой, имя из контента.
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let skills = snap["payload"]["char"]["skills"]
        .as_array()
        .expect("навыки");
    // M17/F30f: + power_strike (имя способности для лога боя) — 34.
    assert_eq!(skills.len(), 34, "все навыки skills.md §3 (уникальные id)");
    let sword = skills
        .iter()
        .find(|s| s["id"] == "sword")
        .expect("sword в снапшоте");
    assert_eq!(sword["xp"], 0, "шкала после повышения пуста");
    assert_eq!(sword["rank"], 1);
    assert_eq!(sword["name"], "Владение мечом");

    // Не хватает очков опыта → E_XP_POOL (шкала полная, пул не тронут).
    sqlx::query("UPDATE characters SET xp_pool = 5 WHERE id = $1")
        .bind(&char_id)
        .execute(&pool)
        .await
        .expect("малый пул");
    common::set_skill(&pool, &char_id, "sword", xp_to_next(1), 1).await;
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_XP_POOL");

    // Гейт атрибутов (§2.2): ранг 9 + шкала 100 % → ранг 10 требует
    // атрибут 12 (у персонажа 10) → E_REQUIREMENT_ATTR, пул не списан.
    sqlx::query("UPDATE characters SET xp_pool = 100000 WHERE id = $1")
        .bind(&char_id)
        .execute(&pool)
        .await
        .expect("пул");
    common::set_skill(&pool, &char_id, "sword", xp_to_next(9), 9).await;
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_REQUIREMENT_ATTR",
        "ранг 10 требует атрибут 12: {ack}"
    );
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    let _ = c.ack(seq).await;
    let snap = c.wait_event("evt/state/snapshot").await;
    assert_eq!(
        snap["payload"]["char"]["xp_pool"], 100000,
        "неудачное обучение не списало пул"
    );

    // Потолок ранга (§2.2): rank = 30 (конфиг) + шкала 100 % → E_RANK_CAP.
    sqlx::query("UPDATE characters SET xp_pool = 1000000 WHERE id = $1")
        .bind(&char_id)
        .execute(&pool)
        .await
        .expect("пул для капа");
    common::set_skill(&pool, &char_id, "sword", xp_to_next(30), 30).await;
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_RANK_CAP", "потолок 30: {ack}");
    pool.close().await;
}

#[tokio::test]
async fn inventory_loot_equip_use_drop() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
    let cfg = common::test_config(&db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");

    // Лимит слотов (D25): забиваем инвентарь → взятие лута блокируется.
    sqlx::query(
        "INSERT INTO items (character_id, def_id) \
         SELECT $1, 'dagger_knife' FROM generate_series(1, 40)",
    )
    .bind(&char_id)
    .execute(&pool)
    .await
    .expect("заполнить слоты");

    let ack = move_exit(&mut c, "e_north").await;
    assert_eq!(ack["result"], "ok", "переход в лес: {ack}");
    let seq = c
        .send("cmd/entity.info", json!({ "entity_id": "cor_9" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "осмотр трупа: {ack}");
    assert_eq!(ack["payload"]["was_player"], true);
    let seq = c
        .send(
            "cmd/item.use",
            json!({ "item_uid": ack["payload"]["loot"][0]["uid"] }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_INVENTORY_FULL",
        "слоты заняты: {ack}"
    );

    // Освобождаем — и идёт основной флоу.
    sqlx::query("DELETE FROM items WHERE character_id = $1 AND def_id = 'dagger_knife'")
        .bind(&char_id)
        .execute(&pool)
        .await
        .expect("очистить слоты");

    // Осмотр: лут статического трупа (§6 extra.loot), взятый — скрыт.
    let seq = c
        .send("cmd/entity.info", json!({ "entity_id": "cor_9" }))
        .await;
    let ack = c.ack(seq).await;
    let loot = ack["payload"]["loot"].as_array().expect("лут").clone();
    assert_eq!(loot.len(), 2);
    assert_eq!(loot[0]["uid"], "loot:cor_9:0");
    assert_eq!(loot[0]["name"], "Кинжал");
    assert_eq!(loot[1]["name"], "Зелье лечения");

    // Взятие: ack пустой + evt/item/added (uid генерирует БД).
    let seq = c
        .send("cmd/item.use", json!({ "item_uid": "loot:cor_9:0" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let added = c.wait_event("evt/item/added").await;
    let knife_uid = added["payload"]["item"]["uid"]
        .as_str()
        .expect("uid")
        .to_string();
    assert!(knife_uid.starts_with("i_"), "uid из БД: {knife_uid}");

    // Повторное взятие того же лута → E_ITEM_GONE (однократно).
    let seq = c
        .send("cmd/item.use", json!({ "item_uid": "loot:cor_9:0" }))
        .await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_ITEM_GONE");
    let seq = c
        .send("cmd/entity.info", json!({ "entity_id": "cor_9" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["payload"]["loot"].as_array().expect("лут").len(),
        1,
        "взятый скрыт"
    );

    // Зелье: взять → использовать → расход (consumed) + патч HP (§7/§8).
    let seq = c
        .send("cmd/item.use", json!({ "item_uid": "loot:cor_9:1" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let added = c.wait_event("evt/item/added").await;
    let potion_uid = added["payload"]["item"]["uid"]
        .as_str()
        .expect("uid")
        .to_string();
    let seq = c
        .send("cmd/item.use", json!({ "item_uid": potion_uid }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "применение зелья");
    let removed = c.wait_event("evt/item/removed").await;
    assert_eq!(removed["payload"]["reason"], "consumed");
    let stats = c.wait_event("evt/char/stats").await;
    assert_eq!(
        stats["payload"]["patch"]["hp"]["cur"], 100,
        "hp не больше максимума"
    );

    // Экипировка: слоты из конфига, переэкипка, требования (§7).
    let seq = c
        .send(
            "cmd/item.equip",
            json!({ "item_uid": knife_uid, "slot": "main_hand" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "надевание: {ack}");
    assert_eq!(ack["payload"]["char"]["equip"]["main_hand"], knife_uid);
    let seq = c
        .send(
            "cmd/item.equip",
            json!({ "item_uid": knife_uid, "slot": "hat" }),
        )
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_ITEM_GONE",
        "слот вне конфига"
    );
    let seq = c
        .send(
            "cmd/item.equip",
            json!({ "item_uid": knife_uid, "slot": "main_hand" }),
        )
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_ITEM_GONE",
        "уже надет"
    );

    let seq = c
        .send("cmd/item.unequip", json!({ "slot": "main_hand" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    assert!(ack["payload"]["char"]["equip"]["main_hand"].is_null());

    // Выброс: evt/item/removed (reason=dropped), снапшот без предмета.
    let seq = c
        .send("cmd/item.drop", json!({ "item_uid": knife_uid }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let removed = c.wait_event("evt/item/removed").await;
    assert_eq!(removed["payload"]["reason"], "dropped");
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    let _ = c.ack(seq).await;
    let snap = c.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    assert!(inv.is_empty(), "инвентарь пуст: {inv:?}");

    // Обычный текст: чат работает с M12.7 — ack + evt/chat/message.
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": "привет" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "чат: {ack}");
    let msg = c
        .wait(|v| v["type"] == "evt/chat/message" && v["payload"]["text"] == "привет")
        .await;
    assert_eq!(msg["payload"]["channel"], "global");
    pool.close().await;
}

#[tokio::test]
async fn death_loses_lut_but_keeps_progress() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
    let cfg = common::test_config(&db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");

    // «Прогресс»: повышение ранга у тренера (M18/F44, ADR-009) — шкала
    // 100 % (SQL), плата из диалога: 10 очков пула + 100 золота.
    sqlx::query("UPDATE characters SET xp_pool = 1000 WHERE id = $1")
        .bind(&char_id)
        .execute(&pool)
        .await
        .expect("пул");
    give_gold(&mut c, 1000).await;
    common::set_skill(&pool, &char_id, "sword", xp_to_next(0), 0).await;
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");

    // «Фарм»: лут в лесу.
    let ack = move_exit(&mut c, "e_north").await;
    assert_eq!(ack["result"], "ok");
    let seq = c
        .send("cmd/item.use", json!({ "item_uid": "loot:cor_9:0" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let added = c.wait_event("evt/item/added").await;
    let knife_uid = added["payload"]["item"]["uid"]
        .as_str()
        .expect("uid")
        .to_string();

    // Смерть (dev-сценарий): теряется только лут (skills.md §2.2).
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": "/scenario death" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "сценарий: {ack}");
    assert!(
        ack["state_version"].is_number(),
        "смерть бампит версию (§3)"
    );

    let death = c.wait_event("evt/char/death").await;
    assert_eq!(death["payload"]["victim_char"], true);
    let lost = death["payload"]["lost_items"].as_array().expect("утрачено");
    // F29a: деньги — обычные предметы: вместе с лутом теряются монеты
    // (остаток 1000 − 100 цены вложения).
    let mut lost_names: Vec<&str> = lost.iter().filter_map(|i| i["name"].as_str()).collect();
    lost_names.sort();
    assert_eq!(
        lost_names,
        ["Золотая монета", "Кинжал"],
        "утрачен весь лут: {lost:?}"
    );
    assert_eq!(death["payload"]["respawn"]["location_id"], "l_village");
    let removed = c
        .wait(|v| v["type"] == "evt/item/removed" && v["payload"]["uid"] == knife_uid)
        .await;
    assert_eq!(removed["payload"]["reason"], "lost_on_death");
    let stats = c.wait_event("evt/char/stats").await;
    assert_eq!(stats["payload"]["patch"]["hp"]["cur"], 100, "респаун полон");
    let changed = c.wait_event("evt/location/changed").await;
    assert_eq!(changed["payload"]["location"]["id"], "l_village");

    // Опыт и пул НЕ теряются (§2.2), инвентарь пуст.
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    let _ = c.ack(seq).await;
    let snap = c.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    assert!(inv.is_empty(), "инвентарь пуст после смерти: {inv:?}");
    assert_eq!(snap["payload"]["char"]["xp_pool"], 990, "пул сохранён");
    let sword = snap["payload"]["char"]["skills"]
        .as_array()
        .expect("навыки")
        .iter()
        .find(|s| s["id"] == "sword")
        .expect("sword")
        .clone();
    assert_eq!(sword["xp"], 0, "шкала обнулена при повышении");
    assert_eq!(sword["rank"], 1, "ранг навыка сохранён");
    assert_eq!(
        snap["payload"]["location"]["id"], "l_village",
        "респаун в спавне"
    );

    // Неизвестный сценарий — ошибка.
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": "/scenario nope" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert!(ack["result"] == "error", "неизвестный сценарий: {ack}");
    pool.close().await;
}

#[tokio::test]
async fn progress_survives_restart() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Первый «сервер»: фарм и прогресс.
    let ws1 = common::start_ws_server(&db_url).await;
    let (mut c, login, char_id) = in_world(&ws1).await;
    let cfg = common::test_config(&db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    // M18/F44: прогресс — шкала 100 % и повышение ранга у тренера
    // (ADR-009): плата 10 очков пула + 100 золота.
    sqlx::query("UPDATE characters SET xp_pool = 500 WHERE id = $1")
        .bind(&char_id)
        .execute(&pool)
        .await
        .expect("пул");
    give_gold(&mut c, 1000).await;
    common::set_skill(&pool, &char_id, "sword", xp_to_next(0), 0).await;
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    // Фарм в лесу — позиция должна пережить рестарт (D27).
    let ack = move_exit(&mut c, "e_north").await;
    assert_eq!(ack["result"], "ok");
    let seq = c
        .send("cmd/item.use", json!({ "item_uid": "loot:cor_9:0" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    c.wait_event("evt/item/added").await;
    drop(c);

    // Второй «сервер» (рестарт): состояние переживает (D27).
    let ws2 = common::start_ws_server(&db_url).await;
    let mut c2 = WsClient::connect(&ws2).await;
    let seq = c2
        .send(
            "cmd/auth.login",
            json!({ "login": login, "password": "longenough1" }),
        )
        .await;
    assert_eq!(c2.ack(seq).await["result"], "ok");
    c2.wait_event("evt/session.start").await;
    // Цепочка reconnect (§4.5): автовосстановленный персонаж + resync.
    let seq = c2
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    let ack = c2.ack(seq).await;
    assert_eq!(ack["result"], "ok", "resync: {ack}");
    let snap = c2.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    // F29a: наряду с лутом инвентарь переживает рестарт и стак монет.
    assert_eq!(inv.len(), 2, "инвентарь пережил рестарт: {inv:?}");
    assert!(
        inv.iter().any(|i| i["type"] == "money" && i["qty"] == 900),
        "монеты пережили рестарт: {inv:?}"
    );
    assert!(
        inv.iter().any(|i| i["type"] == "weapon"),
        "нож пережил рестарт: {inv:?}"
    );
    assert_eq!(
        snap["payload"]["char"]["xp_pool"], 490,
        "пул пережил рестарт (500 − 10 очков)"
    );
    let sword = snap["payload"]["char"]["skills"]
        .as_array()
        .expect("навыки")
        .iter()
        .find(|s| s["id"] == "sword")
        .expect("sword")
        .clone();
    assert_eq!(sword["xp"], 0, "шкала обнулена при повышении");
    assert_eq!(sword["rank"], 1, "ранг пережил рестарт");
    assert_eq!(
        snap["payload"]["location"]["id"], "l_forest",
        "позиция пережила рестарт"
    );
    pool.close().await;
}

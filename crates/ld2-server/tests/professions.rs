//! Профессии и крафт (ФР-08, M12.6): изучение с лимитом, рецепты,
//! ошибки станка/инструмента/ингредиентов, джоб с выдачей по готовности,
//! персистентность материалов через рестарт (D27).

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

/// Начислить золото.
async fn give_gold(c: &mut ld2_test_kit::WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
}

#[tokio::test]
async fn learn_profession_with_limit() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Лимит 1 первичной — третья даёт E_PROFESSION_LIMIT.
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.profession_limit = 1;
        })
        .await
    );
    let (mut c, _login, _char) = in_world(&ws).await;
    // M14: обучение у тренера платное — даём золото (2 × 100).
    give_gold(&mut c, 1000).await;

    // Вторичная профессия лимит не занимает.
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "herbalist" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "вторичная: {ack}");
    assert_eq!(ack["payload"]["profession"]["id"], "herbalist");
    assert_eq!(ack["payload"]["profession"]["rank"], 1);
    c.wait_event("evt/craft/profession").await;

    // Первая первичная — ок.
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "alchemist" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "первая первичная: {ack}");

    // Вторая первичная — лимит.
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "blacksmith" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_PROFESSION_LIMIT", "лимит: {ack}");

    // Неизвестная профессия → E_PROFESSION.
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "wizard" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_PROFESSION", "нет такой: {ack}");

    // Список моих профессий.
    let seq = c.send("cmd/craft.list", json!({})).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let list = ack["payload"]["professions"].as_array().expect("список");
    assert_eq!(list.len(), 2, "herbalist + alchemist: {list:?}");

    // Снапшот несёт изученные профессии.
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let profs = snap["payload"]["char"]["professions"]
        .as_array()
        .expect("professions");
    assert_eq!(profs.len(), 2, "в снапшоте: {profs:?}");
}

#[tokio::test]
async fn craft_requires_profession_station_tool_and_items() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;
    // M14: обучение у тренера платное — даём золото (2 × 100).
    give_gold(&mut c, 1000).await;

    // Рецепты доступны без профессии (красная подсветка — ФР-08).
    let seq = c.send("cmd/craft.recipes", json!({})).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "рецепты: {ack}");
    let recipes = ack["payload"]["recipes"].as_array().expect("recipes");
    assert!(recipes.len() >= 3, "контент рецептов: {recipes:?}");
    let potion = recipes
        .iter()
        .find(|r| r["id"] == "rec_potion_heal")
        .expect("rec_potion_heal")
        .clone();
    assert_eq!(potion["profession"], "alchemist");
    assert_eq!(potion["station"], "alchemy_table");
    assert_eq!(potion["tool"]["type"], "flask");

    // 1) Нет профессии → E_RANK_CAP (ранг проверяется первым).
    let seq = c
        .send(
            "cmd/craft.start",
            json!({ "recipe_id": "rec_potion_heal", "qty": 1 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_RANK_CAP", "нет профессии: {ack}");

    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "alchemist" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    c.wait_event("evt/craft/profession").await;

    // 2) Лес без станков → E_STATION_REQUIRED.
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    let seq = c
        .send(
            "cmd/craft.start",
            json!({ "recipe_id": "rec_potion_heal", "qty": 1 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_STATION_REQUIRED",
        "станка в лесу нет: {ack}"
    );

    // 3) Деревня (спавн, станки есть) → не хватает инструмента.
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_south" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в деревню");
    let seq = c
        .send(
            "cmd/craft.start",
            json!({ "recipe_id": "rec_potion_heal", "qty": 1 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_TOOL_MISSING", "нет колбы: {ack}");

    // 4) Колба есть → не хватает ингредиентов.
    give(&mut c, "tool_flask").await;
    let seq = c
        .send(
            "cmd/craft.start",
            json!({ "recipe_id": "rec_potion_heal", "qty": 1 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_INGREDIENTS", "нет травы: {ack}");

    // 5) Рецепт чужой профессии: ранг проверяется первым → E_RANK_CAP
    // (M20: «Сбор в поле» удалён — трава только из объектов-ресурсов).
    let seq = c
        .send(
            "cmd/craft.start",
            json!({ "recipe_id": "rec_staff_oak", "qty": 1 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_RANK_CAP", "резчик: {ack}");

    // Резчик — вторая первичная (лимит 2 в конфиге — ок).
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "woodcarver" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    c.wait_event("evt/craft/profession").await;
    // Ранг есть → верстак в деревне есть, но нет стамески.
    let seq = c
        .send(
            "cmd/craft.start",
            json!({ "recipe_id": "rec_staff_oak", "qty": 1 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_TOOL_MISSING",
        "нет стамески: {ack}"
    );
}

#[tokio::test]
async fn craft_job_finishes_and_survives_restart() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Рецепт зелья: 3 с, станок/тул/травы — даём заранее.
    let server = common::start_server_guarded(&db_url, |_| {}).await;
    let ws = server.ws.clone();
    let (mut c, login, _char) = in_world(&ws).await;
    // M14: обучение у тренера платное — даём золото.
    give_gold(&mut c, 1000).await;

    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "alchemist" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    c.wait_event("evt/craft/profession").await;

    give(&mut c, "tool_flask").await;
    for _ in 0..4 {
        give(&mut c, "herb").await;
    }

    let seq = c
        .send(
            "cmd/craft.start",
            json!({ "recipe_id": "rec_potion_heal", "qty": 2 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "старт крафта: {ack}");

    // «Рестарт»: гасим первый сервер (тик-луп больше не ворует джоб) —
    // материалы уже списаны, джоб в БД (D27).
    drop(c);
    server.stop().await;
    let ws2 = common::start_ws_server(&db_url).await;
    let mut c2 = ld2_test_kit::WsClient::connect(&ws2).await;
    let seq = c2
        .send(
            "cmd/auth.login",
            json!({ "login": login, "password": "longenough1" }),
        )
        .await;
    assert_eq!(c2.ack(seq).await["result"], "ok");
    c2.wait_event("evt/session.start").await;

    // Готовность: тик-луп шлёт evt/craft/result; джоб = time_s × qty
    // (3 × 2 = 6 с), а wait() ждёт максимум 5 с — опрашиваем дольше.
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let v = c2.recv_json().await;
            if v["type"] == "evt/craft/result" {
                return v;
            }
        }
    })
    .await
    .expect("evt/craft/result");
    assert_eq!(result["payload"]["recipe_id"], "rec_potion_heal");
    assert_eq!(result["payload"]["ok"], true, "успех: {result}");
    let made = result["payload"]["made"].as_array().expect("made");
    assert_eq!(made.len(), 1, "один стак: {made:?}");
    // Рецепт выдаёт 1 зелье × qty=2 → 2 (output.qty × qty).
    assert_eq!(made[0]["qty"], 2, "output 1 × qty 2");
    let item_uid = made[0]["uid"].as_str().expect("uid").to_string();

    // Предмет в инвентаре + XP профессии (65 × 2 = 130 → ранг 2).
    let prof = c2.wait_event("evt/craft/profession").await;
    assert_eq!(prof["payload"]["profession"]["id"], "alchemist");
    let rank = prof["payload"]["profession"]["rank"].as_i64().unwrap_or(0);
    let xp = prof["payload"]["profession"]["xp"].as_i64().unwrap_or(0);
    assert!(
        rank >= 2 || xp >= 65,
        "XP за 2 крафта (rank={rank}, xp={xp}): {prof}"
    );

    let seq = c2
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c2.ack(seq).await["result"], "ok");
    let snap = c2.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    assert!(
        inv.iter().any(|i| i["uid"] == item_uid),
        "выданный предмет в инвентаре: {inv:?}"
    );
}

#[tokio::test]
async fn craft_start_consumes_ingredients_immediately() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    give(&mut c, "herb").await;
    give(&mut c, "herb").await;
    give(&mut c, "tool_flask").await;
    give_gold(&mut c, 1000).await;

    // Учим алхимика; рецепт зелья: 2 травы + колба + станок (в спавне).
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "alchemist" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    c.wait_event("evt/craft/profession").await;

    let seq = c
        .send(
            "cmd/craft.start",
            json!({ "recipe_id": "rec_potion_heal", "qty": 1 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "старт крафта: {ack}");

    // Ингредиенты списаны сразу (трава ×2), колба — инструмент, остаётся.
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    let herbs: i64 = inv
        .iter()
        .filter(|i| i["type"] == "herb")
        .map(|i| i["qty"].as_i64().unwrap_or(1))
        .sum();
    assert_eq!(herbs, 0, "трава списана сразу: {inv:?}");
    assert!(
        inv.iter().any(|i| i["type"] == "flask"),
        "инструмент остаётся: {inv:?}"
    );
}

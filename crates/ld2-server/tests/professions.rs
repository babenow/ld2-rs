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

/// Золото персонажа (сумма стаков монет) — проверка цен M21.
async fn gold_of(db_url: &str, char_id: &str) -> i64 {
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
    .expect("золото");
    pool.close().await;
    gold
}

/// Прямая установка прогресса профессии (уровень/ранг) — условие
/// границы ранга без гринда (M21).
async fn set_prof(db_url: &str, char_id: &str, profession: &str, level: i32, rank: i32) {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    sqlx::query(
        "UPDATE character_professions SET level = $1, rank = $2, xp = 0 \
         WHERE character_id = $3 AND profession_id = $4",
    )
    .bind(level)
    .bind(rank)
    .bind(char_id)
    .bind(profession)
    .execute(&pool)
    .await
    .expect("прогресс профессии");
    pool.close().await;
}

/// Привести квест в состояние `turn_in` (цели выполняются SQL'ем).
async fn set_quest_state(db_url: &str, char_id: &str, quest: &str, state: &str) {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    sqlx::query("UPDATE character_quests SET state = $3 WHERE character_id = $1 AND quest_id = $2")
        .bind(char_id)
        .bind(quest)
        .bind(state)
        .execute(&pool)
        .await
        .expect("состояние квеста");
    pool.close().await;
}

/// M21: повышение ранга — у тренера, по границе уровней и за
/// геометрическую плату `200 × 1.5^ранг`; обучение —500 з.
#[tokio::test]
async fn rank_up_at_trainer_needs_level_and_gold() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
    give_gold(&mut c, 3000).await;

    // Обучение Снятию шкур — дорогая цена профессии (500 з.).
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "skinning" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "learn skinning");
    c.wait_event("evt/craft/profession").await;
    assert_eq!(gold_of(&db_url, &char_id).await, 2500, "обучение500");

    // Уровень0 < 50 → граница не набрана.
    let seq = c
        .send("cmd/craft.rank_up", json!({ "profession_id": "skinning" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_LEVEL_REQUIRED", "граница: {ack}");

    // Вне деревни тренера нет (проверка тренера раньше границы).
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    let seq = c
        .send("cmd/craft.rank_up", json!({ "profession_id": "skinning" }))
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_NO_TRAINER",
        "без тренера"
    );
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_south" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в деревню");

    // Уровень50 → ранг2 за300 з.
    set_prof(&db_url, &char_id, "skinning", 50, 1).await;
    let seq = c
        .send("cmd/craft.rank_up", json!({ "profession_id": "skinning" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "ранг2: {ack}");
    assert_eq!(ack["payload"]["profession"]["rank"], 2);
    assert_eq!(
        ack["payload"]["profession"]["level"], 50,
        "уровень не тронут"
    );
    c.wait_event("evt/craft/profession").await;
    assert_eq!(gold_of(&db_url, &char_id).await, 2200, "ранг2 —300 з.");

    // Граница100 не набрана.
    let seq = c
        .send("cmd/craft.rank_up", json!({ "profession_id": "skinning" }))
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_LEVEL_REQUIRED",
        "граница100"
    );

    // До верха: ранг4 + уровень200 → «Мастер» за1013 з.; дальше — потолок.
    set_prof(&db_url, &char_id, "skinning", 200, 4).await;
    let seq = c
        .send("cmd/craft.rank_up", json!({ "profession_id": "skinning" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "ранг5: {ack}");
    assert_eq!(ack["payload"]["profession"]["rank"], 5);
    c.wait_event("evt/craft/profession").await;
    assert_eq!(gold_of(&db_url, &char_id).await, 1_187, "ранг5 —1013 з.");
    let seq = c
        .send("cmd/craft.rank_up", json!({ "profession_id": "skinning" }))
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_RANK_CAP",
        "высший ранг"
    );
}

/// M21: награда квеста «повышение ранга» — та же граница уровней,
/// но без золота («Волчья беда» → +1 ранг Снятия шкур).
#[tokio::test]
async fn quest_rank_up_reward_respects_level_gate() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
    give_gold(&mut c, 1000).await;

    // Награда требует изученной профессии.
    let seq = c
        .send("cmd/quest.accept", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "приём квеста");
    set_quest_state(&db_url, &char_id, "q_wolf_hunt", "turn_in").await;

    // Не изучена → отказ (квест остаётся «к сдаче»).
    let seq = c
        .send("cmd/quest.turn_in", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_PROFESSION",
        "награда: профессия не изучена"
    );

    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "skinning" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "learn skinning");
    c.wait_event("evt/craft/profession").await;

    // Уровень0 < 50 → сдача блокирована границей.
    let seq = c
        .send("cmd/quest.turn_in", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_LEVEL_REQUIRED",
        "граница награды"
    );

    // Уровень50 → сдача проходит: квест done, ранг2 без траты золота.
    let gold_before = gold_of(&db_url, &char_id).await;
    set_prof(&db_url, &char_id, "skinning", 50, 1).await;
    let seq = c
        .send("cmd/quest.turn_in", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "сдача: {ack}");
    assert_eq!(ack["payload"]["quest"]["state"], "done", "квест завершён");
    let prof = c.wait_event("evt/craft/profession").await;
    assert_eq!(prof["payload"]["profession"]["id"], "skinning");
    assert_eq!(prof["payload"]["profession"]["rank"], 2, "ранг наградой");
    assert_eq!(prof["payload"]["profession"]["level"], 50, "уровень цел");
    assert_eq!(
        gold_of(&db_url, &char_id).await,
        gold_before,
        "награда-ранг бесплатна"
    );
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

    // Вторичная профессия (M21: Кулинария) лимит не занимает.
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "cook" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "вторичная: {ack}");
    assert_eq!(ack["payload"]["profession"]["id"], "cook");
    assert_eq!(ack["payload"]["profession"]["rank"], 1);
    assert_eq!(
        ack["payload"]["profession"]["level"], 0,
        "стартовый уровень"
    );
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
    assert_eq!(list.len(), 2, "cook + alchemist: {list:?}");

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

    // M21: 130 XP → уровень ≥3 (остаток переносится); ранг без
    // тренера остаётся1.
    let prof = c2.wait_event("evt/craft/profession").await;
    assert_eq!(prof["payload"]["profession"]["id"], "alchemist");
    let rank = prof["payload"]["profession"]["rank"].as_i64().unwrap_or(0);
    let level = prof["payload"]["profession"]["level"].as_i64().unwrap_or(0);
    let xp = prof["payload"]["profession"]["xp"].as_i64().unwrap_or(0);
    assert_eq!(rank, 1, "ранг без тренера не растёт: {prof}");
    assert!(
        level >= 3,
        "уровень за 2 крафта (level={level}, xp={xp}): {prof}"
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

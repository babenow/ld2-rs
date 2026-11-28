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

/// M39: тестовый сервер с фиксированным шансом добычи руды.
async fn start_with_mining_chance(db_url: &str, chance: f64) -> String {
    let addr = common::start_server_with(db_url, |cfg| {
        cfg.gameplay.professions.mining_chance_base = chance;
        cfg.gameplay.professions.mining_chance_min = chance;
    })
    .await;
    format!("ws://{addr}/ws")
}

/// M39: волк в лесу агрессивен — урон прерывает сбор (как разделку),
/// поэтому перед добычей его убиваем.
async fn kill_wolf(c: &mut ld2_test_kit::WsClient) {
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "атака волка");
    c.wait_event("evt/corpse/created").await;
    c.wait_event("evt/combat/end").await;
}

/// M33/M34: премиум ускоряет прокачку профессий (×1.5) — сбор руды
/// даёт 65 XP → 98 XP (97.5 с округлением).
#[tokio::test]
async fn premium_boosts_profession_xp_on_gather() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = start_with_mining_chance(&db_url, 1.0).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
    give_gold(&mut c, 1000).await;
    learn(&mut c, "mining").await;

    // Премиум на 30 дней аккаунту персонажа.
    let cfg = common::test_config(&db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("время")
        .as_secs() as i64;
    sqlx::query(
        "UPDATE accounts SET premium_until = $2 \
          WHERE id = (SELECT account_id FROM characters WHERE id = $1)",
    )
    .bind(&char_id)
    .bind(now + 30 * 86_400)
    .execute(&pool)
    .await
    .expect("премиум");
    pool.close().await;

    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    kill_wolf(&mut c).await;
    give(&mut c, "pickaxe").await;

    let seq = c
        .send(
            "cmd/resource.gather",
            json!({ "entity_id": "r_forest_ore" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "руда");
    let prof = c.wait_event("evt/craft/profession").await;
    assert_eq!(prof["payload"]["profession"]["id"], "mining");
    assert_eq!(
        prof["payload"]["gain"], 15,
        "10 × 1.5 = 15 (премиум, медная жила ур.1): {prof}"
    );
}

#[tokio::test]
async fn gather_tool_profession_yield_and_exhaustion() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = start_with_mining_chance(&db_url, 1.0).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
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
    kill_wolf(&mut c).await;

    // Руда ×3: предмет + патч шкалы;4-й — исчерпана (E_TARGET_GONE).
    let seq = c
        .send(
            "cmd/resource.gather",
            json!({ "entity_id": "r_forest_ore" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "первая руда");
    // M39: пока идёт сбор, персонаж занят (как на разделке).
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_south" }))
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_BUSY",
        "движение во время сбора заблокировано"
    );
    // M39: сбор не мгновенный — предмет приходит по завершении джоба.
    let early = tokio::time::timeout(
        std::time::Duration::from_millis(1_500),
        c.wait(|v| v["type"] == "evt/item/added"),
    )
    .await;
    assert!(early.is_err(), "M39: руда не должна выдаваться мгновенно");
    let added = c.wait_event("evt/item/added").await;
    assert_eq!(added["payload"]["item"]["def_id"], "ore_copper");
    // Запас в локации: 3 → 2 (патч entity/move).
    let moved = c.wait_event("evt/entity/move").await;
    assert_eq!(moved["payload"]["entity_id"], "r_forest_ore");
    assert_eq!(moved["payload"]["patch"]["hp"]["cur"], 2);
    // M40: добыча руды — общая строка журнала всем в локации.
    let gathered = c.wait_event("evt/resource/gathered").await;
    assert_eq!(
        gathered["payload"]["char_id"], char_id,
        "кто добыл: {gathered}"
    );
    assert!(
        gathered["payload"]["by_name"]
            .as_str()
            .is_some_and(|n| !n.is_empty()),
        "имя добытчика: {gathered}"
    );
    // M39: завершение сбора — итог как у разделки (клиент гасит бар).
    let result = c.wait_event("evt/harvest/result").await;
    assert_eq!(
        result["payload"]["action"], "gather",
        "итог сбора: {result}"
    );
    assert_eq!(result["payload"]["ok"], true, "сбор завершён: {result}");

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
    assert_eq!(ack["error"]["code"], "E_VEIN_EMPTY", "исчерпана: {ack}");
    assert!(
        ack["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("В жиле нет больше руды")),
        "сообщение пустой жилы: {ack}"
    );

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

    // M39: опыт горного дела обратен шансу (chance=1.0 в тесте → 10 XP);
    // 3 добычи = 30 XP, до уровня 1 (порог 40) не хватает. Ранг остаётся1.
    let profs = snap["payload"]["char"]["professions"]
        .as_array()
        .expect("professions");
    let mining = profs
        .iter()
        .find(|p| p["id"] == "mining")
        .expect("шахтёр в снапшоте");
    assert_eq!(
        mining["level"].as_i64().unwrap_or(0),
        0,
        "уровень за 3 добычи (30/40 XP): {profs:?}"
    );
    assert_eq!(mining["xp"].as_i64().unwrap_or(0), 30, "3×10 XP");
    assert_eq!(mining["rank"], 1, "ранг без тренера не растёт");
    assert_eq!(mining["rank_span"], 50, "границы рангов в extra");
}

/// M39: шанс добычи может провалиться — жила теряет 1 запас, руды
/// нет, в журнал уходит строка «Не удалось добыть руду».
#[tokio::test]
async fn mining_failure_consumes_vein_and_logs() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = start_with_mining_chance(&db_url, 0.0).await;
    let (mut c, _login, _char) = in_world(&ws).await;
    give_gold(&mut c, 1000).await;
    learn(&mut c, "mining").await;
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    kill_wolf(&mut c).await;
    give(&mut c, "pickaxe").await;

    let seq = c
        .send(
            "cmd/resource.gather",
            json!({ "entity_id": "r_forest_ore" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "старт сбора");
    // Запас жилы уменьшился, несмотря на провал.
    let moved = c.wait_event("evt/entity/move").await;
    assert_eq!(moved["payload"]["patch"]["hp"]["cur"], 2);
    let result = c.wait_event("evt/harvest/result").await;
    assert_eq!(result["payload"]["ok"], false, "провал: {result}");
    assert!(
        result["payload"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("Не удалось добыть руду")),
        "строка провала: {result}"
    );

    // Руды в рюкзаке нет.
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    assert!(
        !inv.iter().any(|i| i["def_id"] == "ore_copper"),
        "руда не выдана: {inv:?}"
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

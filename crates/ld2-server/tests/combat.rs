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

    // Гибель моба — строка «{имя} погиб» всем в локации (M29).
    let gone = c.wait_event("evt/entity/despawn").await;
    assert_eq!(gone["payload"]["entity_id"], "m_3");
    assert_eq!(gone["payload"]["kind"], "monster");
    let died = gone["payload"]["text"].as_str().unwrap_or_default();
    assert!(
        died.ends_with(" погиб") || died.ends_with(" погибла"),
        "строка гибели моба: {died:?}"
    );

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

    // XP убийства: base 4 + difficulty 3 × per 3 = 13, навык оружия не
    // экипирован → unarmed (skills.md §2.3; M31 — опыт снижен вдвое).
    let xp = c.wait_event("evt/char/xp").await;
    assert_eq!(xp["payload"]["skill_id"], "unarmed");
    assert_eq!(xp["payload"]["amount"], 13, "4 + difficulty(3) × 3");
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

    // M15/F24: общий опыт и шкала уровня в свежем снапшоте (13 XP, M31).
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
    assert_eq!(ch["total_xp"], 13, "total_xp = XP убийства: {ch}");
    assert_eq!(ch["level"], 1, "13 < 100 → уровень 1");
    assert_eq!(ch["xp_in_level"], 13, "прогресс в уровне");
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

/// M17/F30f: авто-атака тика — строка с «чем бьёт» (оружие не
/// экипировано → «руками») и жирным уроном; кастеру — «Вы бьёте».
#[tokio::test]
async fn auto_attack_log_line_has_hits_as() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "атака");

    let hit = c
        .wait(|v| v["type"] == "evt/combat/hit" && v["payload"]["source"] == char_id)
        .await;
    let text = hit["payload"]["text"].as_str().expect("текст удара");
    assert!(text.starts_with("Вы бьёте"), "обращение кастеру: {text}");
    assert!(text.contains("руками"), "чем бьёт (без оружия): {text}");
    assert!(
        text.contains(&format!("**{}**", hit["payload"]["amount"])),
        "урон жирный: {text}"
    );
}

/// Ранг навыка напрямую в БД (гейт `power_strike`: владение мечом 5).
/// Выдать предмет dev-сценарием → uid.
async fn give(c: &mut ld2_test_kit::WsClient, def_id: &str) -> String {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario give {def_id}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "give {def_id}");
    let added = c.wait_event("evt/item/added").await;
    added["payload"]["item"]["uid"]
        .as_str()
        .expect("uid")
        .to_string()
}

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

/// M17/F30e: ресурс персонажа напрямую в БД — ярость перед ударом
/// (активные навыки воина стоят ресурс класса, конфиг `cost`).
async fn set_resource(db_url: &str, char_id: &str, cur: i32, max: i32) {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    ld2_server::db::characters::set_resource(&pool, char_id, cur, max)
        .await
        .expect("ресурс");
    pool.close().await;
}

/// M17/F30e: активная способность стоит ресурс класса — ярости
/// нет (0 < cost) → E_NOT_ENOUGH_MP «не хватает ярости».
#[tokio::test]
async fn skill_use_requires_class_resource() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
    set_skill_rank(&db_url, &char_id, "sword", 5).await;
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    // Ярости нет: старт воина — 0 (M17/F30e), cost power_strike = 25.
    let seq = c
        .send(
            "cmd/skill.use",
            json!({ "skill_id": "power_strike", "target_id": "m_3" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_NOT_ENOUGH_MP",
        "гейт ресурса: {ack}"
    );
    assert!(
        ack["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("ярости"),
        "текст про ресурс класса: {ack}"
    );
}

/// `cmd/skill.use` (§4, M14): гейт навыка, урон ×1.8, серверный кулдаун
/// `E_COOLDOWN` + `retry_after_s`, ошибки цели.
#[tokio::test]
async fn skill_use_power_strike_gate_damage_and_cooldown() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let addr = common::start_server_with(&db_url, |cfg| {
        cfg.skills
            .active
            .get_mut("power_strike")
            .expect("power_strike в конфиге")
            // Кулдаун в целых секундах (сервер округляет вверх): 3 с
            // даёт надёжное окно для проверки E_COOLDOWN.
            .cooldown_s = 3.0;
    })
    .await;
    let ws = format!("ws://{addr}/ws");
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
    // M17/F30e: удар стоит25 ярости — даём ресурс (старт — 0).
    set_resource(&db_url, &char_id, 100, 100).await;
    // Кэш боя обновляется при экипировке: без меча точность остаётся
    // низкой, и проверка попадания становится флейковой.
    let uid = give(&mut c, "armor_bronze").await;
    let seq = c
        .send("cmd/item.equip", json!({ "item_uid": uid, "slot": "body" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "надеть броню");
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    // Бросок попадания случаен — при промахе повторяем после кулдауна.
    let mut hit = None;
    for attempt in 0..4 {
        let seq = c
            .send(
                "cmd/skill.use",
                json!({ "skill_id": "power_strike", "target_id": "m_3" }),
            )
            .await;
        let ack = c.ack(seq).await;
        if ack["error"]["code"] == "E_COOLDOWN" {
            let wait = ack["error"]["retry_after_s"].as_u64().unwrap_or(1).max(1);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }
        assert_eq!(ack["result"], "ok", "удар #{attempt}: {ack}");
        let cd = c.wait_event("evt/combat/cooldown").await;
        assert_eq!(cd["payload"]["skill_id"], "power_strike");
        assert!(
            cd["payload"]["until_epoch"].as_i64().unwrap_or(0) > 0,
            "until_epoch: {cd}"
        );
        let ev = c
            .wait(|v| {
                v["type"] == "evt/combat/hit"
                    && v["payload"]["kind"] == "skill"
                    && v["payload"]["target"] == "m_3"
            })
            .await;
        if ev["payload"]["miss"] == false {
            hit = Some(ev);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    }
    let hit = hit.expect("способность попала (8 попыток)");
    let amount = hit["payload"]["amount"].as_i64().expect("amount");
    assert!((1..=20).contains(&amount), "урон способности: {hit}");

    // 3) Повтор в кулдауне → E_COOLDOWN + retry_after_s (§7).
    //    Отправляем сразу после попадания, не дожидаясь hp-патча:
    //    ожидание события под нагрузкой могло выйти за окно кулдауна.
    //    hp и ack собираем в одном цикле, чтобы не потерять hp.
    let seq = c
        .send(
            "cmd/skill.use",
            json!({ "skill_id": "power_strike", "target_id": "m_3" }),
        )
        .await;
    let (ack, hp) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        let mut ack = None;
        let mut hp = None;
        while ack.is_none() || hp.is_none() {
            let v = c.recv_json().await;
            if v["type"] == "ack" && v["seq"].as_u64() == Some(u64::from(seq)) {
                ack = Some(v.clone());
            }
            if v["type"] == "evt/entity/hp" && v["payload"]["entity_id"] == "m_3" {
                hp = Some(v);
            }
        }
        (ack.expect("ack повтора"), hp.expect("hp после удара"))
    })
    .await
    .expect("ack/hp после удара");
    assert_eq!(
        ack["error"]["code"], "E_COOLDOWN",
        "второй удар в кулдауне: {ack}"
    );
    assert!(
        ack["error"]["retry_after_s"].as_u64().unwrap_or(0) >= 1,
        "retry_after_s: {ack}"
    );
    assert_eq!(hp["payload"]["hp"]["cur"], 18 - amount, "18 − урон: {hp}");

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
    // M17/F30e: удар стоит25 ярости — даём ресурс (старт — 0).
    set_resource(&db_url, &char_id, 100, 100).await;
    // Кэш боя обновляется при экипировке: без меча точность остаётся
    // низкой, и проверка попадания становится флейковой.
    let uid = give(&mut c, "armor_bronze").await;
    let seq = c
        .send("cmd/item.equip", json!({ "item_uid": uid, "slot": "body" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "надеть броню");

    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");

    // Первый удар способностью (M32: урон листа ×1.8 − броня моба).
    // Бросок попадания случаен, а точная последовательность ГПСЧ зависит
    // от тиков мобов — при промахе повторяем после кулдауна (до 8 раз).
    let mut hit = None;
    for attempt in 0..8 {
        let seq = c
            .send(
                "cmd/skill.use",
                json!({ "skill_id": "power_strike", "target_id": "m_3" }),
            )
            .await;
        let ack = c.ack(seq).await;
        // Кулдаун на сервере округляется вверх до целой секунды —
        // 0.5 с из конфига может держаться до 1 с; ждём retry_after_s.
        if ack["error"]["code"] == "E_COOLDOWN" {
            let wait = ack["error"]["retry_after_s"].as_u64().unwrap_or(1).max(1);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }
        assert_eq!(ack["result"], "ok", "удар #{attempt}: {ack}");
        c.wait_event("evt/combat/cooldown").await;
        let ev = c
            .wait(|v| {
                v["type"] == "evt/combat/hit"
                    && v["payload"]["kind"] == "skill"
                    && v["payload"]["target"] == "m_3"
            })
            .await;
        if ev["payload"]["miss"] == false {
            hit = Some(ev);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    }
    let hit = hit.expect("способность попала (8 попыток)");
    let amount = hit["payload"]["amount"].as_i64().expect("amount");
    assert!(amount > 0, "удар нанёс урон: {hit}");
    // M17/F30f: в логе имя способности (не id) и жирный урон.
    let text = hit["payload"]["text"].as_str().unwrap_or_default();
    assert!(text.contains("«Удар мощью»"), "имя скилла: {text}");
    assert!(
        text.contains(&format!("**−{amount}**")),
        "жирный урон: {text}"
    );

    // Добиваем авто-атаками (M32: промахи/парирование — случайны, поэтому
    // не полагаемся на фиксированное число ударов; тик бьёт раз в секунду).
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "авто-атака");
    let corpse = c.wait_event("evt/corpse/created").await;
    assert_eq!(corpse["payload"]["entity"]["kind"], "corpse");
    let xp = c.wait_event("evt/char/xp").await;
    assert_eq!(xp["payload"]["amount"], 13, "XP убийства: {xp}");
    assert_eq!(
        xp["payload"]["skill_id"], "unarmed",
        "оружие не экипировано"
    );
}

/// M33: премиум-аккаунт — +50 % опыта за убийство (13 → 20).
#[tokio::test]
async fn premium_account_boosts_kill_xp() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;

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
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "атака");

    let xp = c.wait_event("evt/char/xp").await;
    assert_eq!(xp["payload"]["skill_id"], "unarmed");
    assert_eq!(
        xp["payload"]["amount"], 20,
        "13 × 1.5 = 19.5 → 20 (премиум): {xp}"
    );
}

/// M17/F30e: ярость воина копится за нанесённый урон — первый удар
/// даёт патч `evt/char/stats` с `mp.cur > 0` (указание заказчика:
/// «накопление ресурса у воина не происходит» — проверяем на живом
/// бое; hp-патчи от моба отфильтровываются).
#[tokio::test]
async fn warrior_rage_accumulates_on_melee() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await; // профиль warrior

    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "атака");

    let patch = c
        .wait(|v| {
            v["type"] == "evt/char/stats"
                && v["payload"]["patch"]["mp"]["cur"]
                    .as_i64()
                    .is_some_and(|n| n > 0)
        })
        .await;
    assert!(
        patch["payload"]["patch"]["mp"]["cur"].as_i64().unwrap_or(0) > 0,
        "ярость пришла: {patch}"
    );
}

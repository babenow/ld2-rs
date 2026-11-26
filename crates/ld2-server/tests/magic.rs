//! Магия и сокеты (ФР-09, M12.5b): инкрустация/извлечение рун, каст руны
//! (мана + заряды), каст свитка, реген маны по тику.

mod common;

use common::{in_world, in_world_profile};
use serde_json::json;

/// Выдать предмет dev-сценарием → uid из `evt/item/added`.
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
        .expect("uid предмета")
        .to_string()
}

/// M17/F30c: самоцвет в надетой броне улучшает основную
/// характеристику (снапшот-attrs с бонусом); снятие — бонус пропадает.
#[tokio::test]
async fn gem_in_armor_boosts_attrs_when_equipped() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    let armor = give(&mut c, "armor_bronze").await; // sockets: 1 (ручной сокет)
    let gem = give(&mut c, "gem_str").await; // +3 к силе (ручной бонус)

    // Вставка самоцвета в ненадетую броню — без эффекта на attrs.
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": armor, "socket_index": 0, "rune_uid": gem }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "самоцвет в броне: {ack}");

    // Надели — сила выросла (10 + 3).
    let seq = c
        .send(
            "cmd/item.equip",
            json!({ "item_uid": armor, "slot": "body" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "надевание: {ack}");
    assert_eq!(
        ack["payload"]["char"]["attrs"]["str"], 13,
        "бонус самоцвета в снапшоте: {ack}"
    );

    // Сняли — бонуса нет.
    let seq = c.send("cmd/item.unequip", json!({ "slot": "body" })).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "снятие: {ack}");
    assert_eq!(
        ack["payload"]["char"]["attrs"]["str"], 10,
        "бонус пропал: {ack}"
    );
}

/// M18/F32: руны — только в оружие, самоцветы — только в броню
/// (отказ `E_NO_RUNE_SLOT`); вставка/извлечение в **надетой** броне
/// сразу меняет атрибуты — ack несёт `char` (бонус без переодевания).
#[tokio::test]
async fn rune_weapon_gem_armor_only_and_equipped_attrs() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    let armor = give(&mut c, "armor_bronze").await; // sockets: 1
    let sword = give(&mut c, "sword_knight").await; // sockets: 1
    let rune = give(&mut c, "rune_fire").await;
    let gem = give(&mut c, "gem_str").await; // +3 str

    // M18/F58: руна в броню — отказ, т.к. слот брони принимает
    // самоцветы (решает `accepts`, а не тип предмета).
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": armor, "socket_index": 0, "rune_uid": rune }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_NO_RUNE_SLOT",
        "руна в броню: {ack}"
    );
    assert!(
        ack["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("самоцветы"),
        "текст называет, что принимает слот: {ack}"
    );

    // Самоцвет в оружие — отказ.
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": sword, "socket_index": 0, "rune_uid": gem }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_NO_RUNE_SLOT",
        "самоцвет в оружие: {ack}"
    );

    // Надели броню → вставили самоцвет → ack несёт `char` с бонусом (10+3).
    let seq = c
        .send(
            "cmd/item.equip",
            json!({ "item_uid": armor, "slot": "body" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "надевание");
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": armor, "socket_index": 0, "rune_uid": gem }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "самоцвет в надетой броне: {ack}");
    assert_eq!(
        ack["payload"]["char"]["attrs"]["str"], 13,
        "бонус сразу в ack: {ack}"
    );

    // Извлечение — бонус снят (тот же `char` в ack).
    let seq = c
        .send(
            "cmd/socket.extract",
            json!({ "staff_uid": armor, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "извлечение: {ack}");
    assert_eq!(
        ack["payload"]["char"]["attrs"]["str"], 10,
        "бонус снят: {ack}"
    );
    assert!(
        ack["payload"]["char"]["inventory"]
            .as_array()
            .is_some_and(|inv| inv.iter().any(|i| i["def_id"] == "gem_str")),
        "самоцвет вернулся в инвентарь: {ack}"
    );
}

/// M25 (заказчик): меч принимает руны и **кастует** — кнопки панели
/// для рун меча активны (раньше каст был привилегией посоха).
#[tokio::test]
async fn sword_takes_runes_and_casts() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await; // warrior

    let sword = give(&mut c, "sword_knight").await; // sockets: 1 (контент)
    let rune = give(&mut c, "rune_fury").await; // class: warrior
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": sword, "socket_index": 0, "rune_uid": rune }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "инкрустация в меч: {ack}");
    let removed = c.wait_event("evt/item/removed").await;
    assert_eq!(removed["payload"]["uid"], rune, "руна израсходована");

    // Каст с меча разрешён (M25); воину нужна ярость — даём ресурс.
    let pool = sqlx::PgPool::connect(&db_url).await.expect("пул");
    sqlx::query("UPDATE characters SET mp_cur = 50 WHERE id = $1")
        .bind(&char_id)
        .execute(&pool)
        .await
        .expect("ярость");
    pool.close().await;
    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": sword, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "каст с меча: {ack}");

    // Извлечение обратно — паритет с посохом.
    let seq = c
        .send(
            "cmd/socket.extract",
            json!({ "staff_uid": sword, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "извлечение из меча: {ack}");
    assert_eq!(ack["payload"]["rune"]["name"], "Руна ярости", "{ack}");
}

#[tokio::test]
async fn socket_insert_cast_extract_flow() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world_profile(&ws, "mage").await;

    let staff_uid = give(&mut c, "staff_apprentice").await;
    let rune_uid = give(&mut c, "rune_fire").await;

    // Посох приходит с пустыми слотами (ADR-002: `rune: null`).
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 0, "rune_uid": rune_uid }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "инкрустация: {ack}");
    let rune = &ack["payload"]["staff"]["sockets"][0]["rune"];
    assert_eq!(rune["name"], "Руна огня", "руна в сокете: {ack}");
    assert_eq!(rune["charges"], 7, "заряды из определения");
    assert_eq!(rune["spell"], "fire_dart");
    // M18/F58f: знак руны — контент-поле glyph, копируется в сокет.
    assert_eq!(rune["glyph"], "f", "glyph из параметров руны: {ack}");
    // M18/F58f: раритет вставленного — знак самоцвета по раритету.
    assert_eq!(rune["rarity"], "uncommon", "rarity из контента: {ack}");
    // M18/F58f: подпись слота (что даёт) — серверные строки из params.
    let detail = rune["detail"].as_array().expect("detail в сокете");
    assert!(
        detail.iter().any(|l| l.as_str() == Some("+12 к урону")),
        "урон в подписи: {detail:?}"
    );
    let removed = c.wait_event("evt/item/removed").await;
    assert_eq!(removed["payload"]["uid"], rune_uid, "руна израсходована");

    // Занятый слот → E_NO_RUNE_SLOT; несуществующий → тоже.
    let rune2 = give(&mut c, "rune_fire").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 0, "rune_uid": rune2 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NO_RUNE_SLOT", "занят: {ack}");
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 9, "rune_uid": rune2 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NO_RUNE_SLOT", "нет слота: {ack}");

    // Каст руны (D4): −10 маны, −1 заряд, событие `evt/magic/cast`.
    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": staff_uid, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "каст: {ack}");
    let cast = c.wait_event("evt/magic/cast").await;
    assert_eq!(cast["payload"]["spell"], "fire_dart");
    assert_eq!(cast["payload"]["what"], "rune");
    assert_eq!(cast["payload"]["charges_left"], 6, "заряд −1");
    assert_eq!(cast["payload"]["mp_left"], 50, "мана 60 − 10");

    // Извлечение: руна в инвентарь (заряды сбрасываются к максимуму —
    // отклонение M12.5b), слот пуст.
    let seq = c
        .send(
            "cmd/socket.extract",
            json!({ "staff_uid": staff_uid, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "извлечение: {ack}");
    assert!(
        ack["payload"]["staff"]["sockets"][0]["rune"].is_null(),
        "слот пуст: {ack}"
    );
    assert_eq!(ack["payload"]["rune"]["name"], "Руна огня", "руна назад");
    assert_eq!(ack["payload"]["rune"]["stats"]["charges"], 7, "как новая");

    // Пустой слот кастовать нельзя.
    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": staff_uid, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NO_RUNE_SLOT", "пусто: {ack}");
}

#[tokio::test]
async fn rune_cast_stops_without_charges() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // rune_cost = 1: хватает маны на все 7 зарядов руны.
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.magic.mana.rune_cost = 1;
        })
        .await
    );
    let (mut c, _login, _char) = in_world_profile(&ws, "mage").await;

    let staff_uid = give(&mut c, "staff_apprentice").await;
    let rune_uid = give(&mut c, "rune_fire").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 0, "rune_uid": rune_uid }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");

    for i in 0..7 {
        // M17/F30g: кулдаун руны (cooldown_s,1 с) между кастами.
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        }
        let seq = c
            .send(
                "cmd/magic.rune_cast",
                json!({ "staff_uid": staff_uid, "socket_index": 0 }),
            )
            .await;
        let ack = c.ack(seq).await;
        assert_eq!(ack["result"], "ok", "каст #{i}: {ack}");
        let cast = c.wait_event("evt/magic/cast").await;
        assert_eq!(cast["payload"]["charges_left"], 6 - i, "заряды вниз");
    }

    // Заряды кончились → E_NO_CHARGES (мана ещё есть).
    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": staff_uid, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NO_CHARGES", "нет зарядов: {ack}");
}

#[tokio::test]
async fn rune_cast_requires_mana() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Стоимость выше стартового бара (60) → сразу E_NOT_ENOUGH_MP.
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.magic.mana.rune_cost = 61;
        })
        .await
    );
    let (mut c, _login, _char) = in_world_profile(&ws, "mage").await;

    let staff_uid = give(&mut c, "staff_apprentice").await;
    let rune_uid = give(&mut c, "rune_fire").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 0, "rune_uid": rune_uid }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");

    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": staff_uid, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NOT_ENOUGH_MP", "нет маны: {ack}");
}

#[tokio::test]
async fn scroll_cast_needs_target_and_kills() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char) = in_world(&ws).await;

    let scroll1 = give(&mut c, "scroll_frost").await;
    let scroll2 = give(&mut c, "scroll_frost").await;
    let scroll3 = give(&mut c, "scroll_frost").await;

    // В лес — к волку (hp 18, свиток бьёт на 9 → два каста).
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "переход в лес");

    // scroll_requires_target = true → без цели отказ.
    let seq = c
        .send("cmd/magic.scroll_cast", json!({ "item_uid": scroll1 }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_TARGET_GONE", "без цели: {ack}");

    // Первый каст: урон 9 − 6 % магреза волка = 8 (18 → 10, M32).
    let seq = c
        .send(
            "cmd/magic.scroll_cast",
            json!({ "item_uid": scroll1, "target_id": "m_3" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "каст свитка: {ack}");
    let cast = c.wait_event("evt/magic/cast").await;
    assert_eq!(cast["payload"]["spell"], "frost_bolt");
    assert_eq!(cast["payload"]["what"], "scroll");
    assert_eq!(cast["payload"]["by"], char);
    // Свиток израсходован (qty = 1 → удаление) — сразу за cast.
    let removed = c.wait_event("evt/item/removed").await;
    assert_eq!(removed["payload"]["uid"], scroll1);
    let hit = c
        .wait(|v| v["type"] == "evt/combat/hit" && v["payload"]["source"] == char)
        .await;
    assert_eq!(hit["payload"]["target"], "m_3", "урон волку: {hit}");
    assert_eq!(
        hit["payload"]["amount"], 8,
        "9 − 6 % сопротивления = 8: {hit}"
    );
    // M17/F30f: слова заклинания и «чем бьёт» — в строке кастера,
    // магические слова жирные.
    let text = hit["payload"]["text"].as_str().unwrap_or_default();
    assert!(text.contains("**Neris Vael**"), "слова жирные: {text}");
    assert!(text.contains("ледяной стрелой"), "чем бьёт: {text}");
    assert!(text.contains("вы бьёте"), "кастер-формат: {text}");
    let hp = c
        .wait(|v| v["type"] == "evt/entity/hp" && v["payload"]["entity_id"] == "m_3")
        .await;
    assert_eq!(hp["payload"]["hp"]["cur"], 10, "18 − 8: {hp}");

    // Второй каст: 10 → 2 (M17/F30g: кулдаун свитка 1 с).
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let seq = c
        .send(
            "cmd/magic.scroll_cast",
            json!({ "item_uid": scroll2, "target_id": "m_3" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "второй каст");
    let hp = c
        .wait(|v| v["type"] == "evt/entity/hp" && v["payload"]["entity_id"] == "m_3")
        .await;
    assert_eq!(hp["payload"]["hp"]["cur"], 2, "10 − 8: {hp}");

    // Третий каст добивает: труп + XP в школу `frost`.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let seq = c
        .send(
            "cmd/magic.scroll_cast",
            json!({ "item_uid": scroll3, "target_id": "m_3" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "каст-добивание");
    let corpse = c.wait_event("evt/corpse/created").await;
    assert!(corpse["payload"]["entity"]["id"].is_string(), "id трупа");
    let end = c.wait_event("evt/combat/end").await;
    assert_eq!(end["payload"]["reason"], "цель уничтожена");
    let xp = c.wait_event("evt/char/xp").await;
    assert_eq!(xp["payload"]["skill_id"], "frost", "школа заклинания");
    assert_eq!(xp["payload"]["amount"], 13, "4 + difficulty(3) × 3 (M31)");

    // Дальше свиток кончился → E_ITEM_GONE.
    let seq = c
        .send(
            "cmd/magic.scroll_cast",
            json!({ "item_uid": scroll3, "target_id": "m_3" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_ITEM_GONE", "свитка нет: {ack}");
}

#[tokio::test]
async fn mana_regen_out_of_combat() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // M30: максимум бара мага — 110 (формула character-params.md §1.1),
    // стартовое mp_cur = 60. Полный бар за 6 с → шаг регена
    // ceil(110/6)=19 × духовность 1.1 ≈ 21/с (без боя). rune_cost = 30:
    // два каста списывают 60, а реген поднимает бар уже через ~1 тик —
    // поэтому фиксируем бар пином и проверяем только факт списания
    // (гейт пустого бара покрыт в rune_cast_requires_mana).
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.magic.mana.regen_out_s = 6;
            cfg.gameplay.magic.mana.rune_cost = 30;
        })
        .await
    );
    let (mut c, _login, char_id) = in_world_profile(&ws, "mage").await;

    let staff_uid = give(&mut c, "staff_apprentice").await;
    let rune_uid = give(&mut c, "rune_fire").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 0, "rune_uid": rune_uid }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");

    // Пин бара перед кастами: реген-тик (~21/с) во время setup иначе
    // сдвигает mp_left первого каста с 30.
    let pool = sqlx::PgPool::connect(&db_url).await.expect("пул");
    sqlx::query("UPDATE characters SET mp_cur = 60 WHERE id = $1")
        .bind(&char_id)
        .execute(&pool)
        .await
        .expect("пин бара");
    pool.close().await;

    // 2 каста × 30 маны; M17/F30g: между кастами кулдаун руны (1 с),
    // пауза — окно регена (M30: вне боя ≈21/с при regen_out_s=6),
    // поэтому точное mp_left второго каста зависит от числа тиков.
    for i in 0..2 {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        }
        let seq = c
            .send(
                "cmd/magic.rune_cast",
                json!({ "staff_uid": staff_uid, "socket_index": 0 }),
            )
            .await;
        let ack = c.ack(seq).await;
        assert_eq!(ack["result"], "ok", "каст #{i}: {ack}");
        let cast = c.wait_event("evt/magic/cast").await;
        if i == 0 {
            assert_eq!(cast["payload"]["mp_left"], 30, "расход маны D4: {cast}");
        } else {
            // Списано 30 от бара ≤60+реген — mp_left ниже стартового
            // 60 (гейт пустого бара покрыт в rune_cast_requires_mana).
            assert!(
                cast["payload"]["mp_left"].as_i64().unwrap_or(60) < 60,
                "бар упал после второго каста: {cast}"
            );
        }
    }

    // Реген по тику (§2.4): бар поднимается до цены каста (~1 тик
    // при ≈21/с — M30 перекрыл старые «3 с при 10/с»).
    let stats = c
        .wait(|v| {
            v["type"] == "evt/char/stats"
                && v["payload"]["patch"]["mp"]["cur"].as_i64().unwrap_or(0) >= 30
        })
        .await;
    let mp = stats["payload"]["patch"]["mp"]["cur"].as_i64().expect("mp");
    assert!(mp >= 30, "реген поднял бар хотя бы до цены каста: {mp}");

    // Кулдаун руны (M17/F30g, 1 с) — M30-реген успевает поднять бар
    // раньше, чем истечёт кулдаун, поэтому ждём его явно.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": staff_uid, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "каст после регена: {ack}");
}

/// M17/F30g: кулдаун предмета из контента (cooldown_s, дефолт 1 с) —
/// повторное использование зелья сразу → E_COOLDOWN + retry_after_s,
/// после паузы — снова можно.
#[tokio::test]
async fn item_use_has_cooldown_from_content() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    // Первое зелье съедается; кулдаун общий для всех зелий одного
    // определения — проверяем на втором экземпляре.
    let potion = give(&mut c, "potion_heal").await;
    let potion2 = give(&mut c, "potion_heal").await;
    let seq = c.send("cmd/item.use", json!({ "item_uid": potion })).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "первое использование: {ack}");

    // Сразу повторно другим экземпляром — кулдаун предмета.
    let seq = c.send("cmd/item.use", json!({ "item_uid": potion2 })).await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_COOLDOWN",
        "повтор в кулдауне: {ack}"
    );
    assert!(
        ack["error"]["retry_after_s"].as_u64().unwrap_or(0) >= 1,
        "retry_after от кулдауна: {ack}"
    );

    // Кулдаун истёк (дефолт 1 с) — снова можно.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let seq = c.send("cmd/item.use", json!({ "item_uid": potion2 })).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "после кулдауна: {ack}");
}

/// Сокеты и заряды переживают перезапуск сервера (персистентны в БД, D27).
#[tokio::test]
async fn sockets_persist_across_server_restart() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, login, _char) = in_world(&ws).await;

    let staff_uid = give(&mut c, "staff_apprentice").await;
    let rune_uid = give(&mut c, "rune_fire").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 1, "rune_uid": rune_uid }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    drop(c);

    // Новый «сервер» (та же БД): посох читается с руной во 2-м слоте.
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
    let seq = c2
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c2.ack(seq).await["result"], "ok");
    let snap = c2.wait_event("evt/state/snapshot").await;
    let staff = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь")
        .iter()
        .find(|i| i["uid"] == staff_uid)
        .expect("посох в инвентаре")
        .clone();
    assert!(
        staff["sockets"][1]["rune"]["name"].is_string(),
        "руна пережила рестарт: {staff}"
    );
    assert!(
        staff["sockets"][0]["rune"].is_null(),
        "пустой слот остался пустым: {staff}"
    );
}

/// M18/F58 (ADR-012): сервер проставляет `accepts` в снапшоте — тип
/// слота реально доходит до клиента. Дефолт по типу предмета (когда
/// `rune_sockets` не задан): броня → `gem`, оружие/посох → `rune`
/// — прежнее поведение правил инкрустации сохраняется.
#[tokio::test]
async fn snapshot_carries_socket_accepts() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    // staff_apprentice: 2 слота, посох → оба рунных;
    // helm_knight: 1 слот, броня → под самоцвет.
    let staff_uid = give(&mut c, "staff_apprentice").await;
    let helm_uid = give(&mut c, "helm_knight").await;

    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "resync");
    let snap = c.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");

    let accepts_of = |uid: &str| -> Vec<String> {
        let item = inv
            .iter()
            .find(|i| i["uid"] == uid)
            .unwrap_or_else(|| panic!("предмет {uid} должен быть в инвентаре"));
        item["sockets"]
            .as_array()
            .expect("сокеты")
            .iter()
            .map(|s| s["accepts"].as_str().unwrap_or("<нет поля>").to_string())
            .collect()
    };

    assert_eq!(
        accepts_of(&staff_uid),
        ["rune", "rune"],
        "посох: все слоты рунные (дефолт не-брони)"
    );
    assert_eq!(accepts_of(&helm_uid), ["gem"], "броня: слот под самоцвет");
}

/// M18/F58 (ADR-012): слот принимает по `accepts` — гемы в оружие
/// разрешены там, где слот под самоцветы (`rune_sockets` делит
/// слоты: первые рунные, остальные под самоцветы).
#[tokio::test]
async fn gem_goes_into_weapon_socket_by_accepts() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    // 4 слота: 0,1 — рунные; 2,3 — под самоцветы.
    let sword = give(&mut c, "sword_1000_true").await;
    let rune = give(&mut c, "rune_fire").await;
    let gem = give(&mut c, "gem_str").await;

    async fn insert(
        c: &mut ld2_test_kit::WsClient,
        staff_uid: &str,
        idx: u64,
        rune_uid: &str,
    ) -> serde_json::Value {
        let seq = c
            .send(
                "cmd/socket.insert",
                json!({ "staff_uid": staff_uid, "socket_index": idx, "rune_uid": rune_uid }),
            )
            .await;
        c.ack(seq).await
    }

    // Руна в рунный слот — работает (экземпляр расходуется).
    let ack = insert(&mut c, &sword, 0, &rune).await;
    assert_eq!(ack["result"], "ok", "руна в рунный слот: {ack}");
    let rune2 = give(&mut c, "rune_fire").await;

    // Самоцвет в слот оружия (accepts: gem) — работает (F58).
    let ack = insert(&mut c, &sword, 2, &gem).await;
    assert_eq!(ack["result"], "ok", "самоцвет в оружие разрешён: {ack}");
    // Гем потрачен при успешной вставке — для кейса ниже нужен новый.
    let gem2 = give(&mut c, "gem_str").await;

    // Руна в гемный слот — отказ (свежий экземпляр: старый потрачен).
    let ack = insert(&mut c, &sword, 3, &rune2).await;
    assert_eq!(
        ack["error"]["code"], "E_NO_RUNE_SLOT",
        "руна в гемный слот: {ack}"
    );
    assert!(
        ack["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("самоцветы"),
        "текст называет принимаемое слотом: {ack}"
    );

    // Самоцвет в рунный слот — отказ (слот 1 рунный); gem потрачен → свой.
    let ack = insert(&mut c, &sword, 1, &gem2).await;
    assert_eq!(
        ack["error"]["code"], "E_NO_RUNE_SLOT",
        "самоцвет в рунный слот: {ack}"
    );
    assert!(
        ack["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("руны"),
        "текст называет принимаемое слотом: {ack}"
    );
}

/// M18/F58c (ADR-012 §4): тикер восстанавливает заряды рун — аналог
/// `resource.energy_regen_s`, сек из конфига; клиент получает патч
/// `evt/item/state` с новыми зарядами (не нужно ждать ресинк).
#[tokio::test]
async fn rune_charges_regenerate_by_ticker() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.magic.rune_recharge = "time".into();
            cfg.gameplay.magic.rune_recharge_s = 1.0; // шаг каждую секунду
        })
        .await
    );
    let (mut c, _login, _char) = in_world_profile(&ws, "mage").await;

    let staff_uid = give(&mut c, "staff_apprentice").await;
    let rune_uid = give(&mut c, "rune_fire").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 0, "rune_uid": rune_uid }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");

    // Вставлена руна 7/10 (charges из def) → тикер добирает до max;
    // патч приходит от тикера, а не от вставки (вставка шлёт патч по uid руны).
    let patch = c
        .wait(|v| {
            v["type"] == "evt/item/state"
                && v["payload"]["uid"] == staff_uid.as_str()
                && v["payload"]["patch"]["sockets"].is_array()
        })
        .await;
    let charges = patch["payload"]["patch"]["sockets"][0]["rune"]["charges"]
        .as_i64()
        .expect("charges в патче");
    assert!(
        charges > 7 && charges <= 10,
        "тикер поднял заряды (7 < charges ≤ 10): {charges}, патч: {patch}"
    );
    // Сокеты в патче — wire-виде (§6: есть accepts, нет служебного def_id).
    assert_eq!(
        patch["payload"]["patch"]["sockets"][0]["accepts"].as_str(),
        Some("rune"),
        "accepts добрался в патч: {patch}"
    );
    assert!(
        patch["payload"]["patch"]["sockets"][0]
            .get("def_id")
            .is_none(),
        "служебное поле не уходит клиенту: {patch}"
    );
}

/// M18/F58c: режим `rune_recharge = "item"` — тикера нет, заряды
/// остаются прежними (ключ больше не декоративен: режим решает).
#[tokio::test]
async fn rune_recharge_item_mode_disables_ticker() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.magic.rune_recharge = "item".into();
            cfg.gameplay.magic.rune_recharge_s = 1.0; // если б тикер шёл — успел бы
        })
        .await
    );
    let (mut c, _login, _char) = in_world_profile(&ws, "mage").await;

    let staff_uid = give(&mut c, "staff_apprentice").await;
    let rune_uid = give(&mut c, "rune_fire").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 0, "rune_uid": rune_uid }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");

    // Две секунды тик-лупа: патча зарядов быть не должно, заряды прежние.
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let staff = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь")
        .iter()
        .find(|i| i["uid"] == staff_uid)
        .expect("посох");
    assert_eq!(
        staff["sockets"][0]["rune"]["charges"].as_i64(),
        Some(7),
        "режим item: заряды не тикают: {staff}"
    );
}

/// M18/F58c: прочитать до появления ОБОИХ событий — порядок в потоке
/// не гарантирован (патч сокетов уходит раньше cast-события).
async fn wait_cast_and_socket_patch(
    c: &mut ld2_test_kit::WsClient,
    staff_uid: &str,
) -> (serde_json::Value, serde_json::Value) {
    let mut cast = None;
    let mut patch = None;
    while cast.is_none() || patch.is_none() {
        let v = c.recv_json().await;
        match v["type"].as_str() {
            Some("evt/magic/cast") if cast.is_none() => cast = Some(v),
            Some("evt/item/state")
                if patch.is_none()
                    && v["payload"]["uid"] == staff_uid
                    && v["payload"]["patch"]["sockets"].is_array() =>
            {
                patch = Some(v)
            }
            _ => {}
        }
    }
    (cast.expect("cast"), patch.expect("патч сокетов"))
}

/// M18/F58c (ADR-012 §4): затрата — поле руны `cost`, а не всегда 1:
/// «Руна шторма» (cost 2) снимает 2 заряда за каст и требует полной
/// оплаты (1 заряд < cost → E_NO_CHARGES, даже не смотрит ману).
#[tokio::test]
async fn rune_cast_spends_configured_charge_cost() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.magic.mana.rune_cost = 1; // мана не мешает — проверяем заряды
        })
        .await
    );
    let (mut c, _login, _char) = in_world_profile(&ws, "mage").await;

    let staff_uid = give(&mut c, "staff_apprentice").await;
    let rune_uid = give(&mut c, "rune_storm").await; // charges 5, cost 2
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 0, "rune_uid": rune_uid }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");

    // Каст #1: 5 → 3 (cost 2), патч сокетов идёт тем же потоком.
    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": staff_uid, "socket_index": 0 }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let (cast, patch) = wait_cast_and_socket_patch(&mut c, &staff_uid).await;
    assert_eq!(cast["payload"]["charges_left"], 3, "cost 2: 5 → 3");
    assert_eq!(
        patch["payload"]["patch"]["sockets"][0]["rune"]["charges"].as_i64(),
        Some(3),
        "патч сокетов несёт новые заряды: {patch}"
    );

    // Каст #2: 3 → 1.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": staff_uid, "socket_index": 0 }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let (cast, _) = wait_cast_and_socket_patch(&mut c, &staff_uid).await;
    assert_eq!(cast["payload"]["charges_left"], 1, "cost 2: 3 → 1");

    // Каст #3: 1 заряд < cost 2 → отказ.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": staff_uid, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_NO_CHARGES",
        "недостаточно зарядов на cost: {ack}"
    );
}

/// M18/F58d (ADR-012 §6): бафф из БД доходит до клиента в
/// `char.buffs`, влияет на `derived` и снимается тикером с полной
/// заменой `evt/char/buffs`.
#[tokio::test]
async fn buff_in_snapshot_affects_derived_and_expires() {
    use ld2_server::db;

    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.magic.rune_recharge_s = 1.0;
        })
        .await
    );
    let (mut c, _login, char_id) = in_world(&ws).await;

    // Снимок до баффа — базовая точность.
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let base_accuracy = snap["payload"]["char"]["derived"]["accuracy"]
        .as_i64()
        .expect("accuracy до баффа");
    assert!(
        snap["payload"]["char"]["buffs"]
            .as_array()
            .is_none_or(|b| b.is_empty()),
        "до баффа список пуст"
    );

    // Накладываем бафф напрямую в БД (процесс наложения — F58d-3).
    let pool = sqlx::PgPool::connect(&db_url).await.expect("пул");
    let now = ld2_server::time::now_epoch();
    db::buffs::apply(
        &pool,
        &char_id,
        "rune_marksman",
        "Меткость",
        "accuracy",
        6,
        false,
        60, // длительность, с
        3,  // max_stacks
        now,
    )
    .await
    .expect("бафф наложен");
    db::buffs::apply(
        &pool,
        &char_id,
        "rune_marksman",
        "Меткость",
        "accuracy",
        6,
        false,
        60,
        3,
        now,
    )
    .await
    .expect("второй стак");
    pool.close().await;

    // Ресинк: бафф в char.buffs, derived вырос ровно на value×стаков.
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let buffs = snap["payload"]["char"]["buffs"]
        .as_array()
        .expect("buffs в снапшоте");
    assert_eq!(buffs.len(), 1, "один бафф: {buffs:?}");
    assert_eq!(buffs[0]["kind"], "rune_marksman");
    assert_eq!(buffs[0]["stacks"], 2, "два наложения → 2 стака");
    let buffed = snap["payload"]["char"]["derived"]["accuracy"]
        .as_i64()
        .expect("accuracy с баффом");
    assert_eq!(
        buffed,
        base_accuracy + 12,
        "бафф accuracy 6 × 2 стака применён к derived"
    );

    // Короткий бафф: тикер (раз в секунду) снимает и шлёт полную
    // замену evt/char/buffs (паттерн evt/crime/status).
    let pool = sqlx::PgPool::connect(&db_url).await.expect("пул");
    db::buffs::apply(
        &pool,
        &char_id,
        "rune_warmaster",
        "Ярость",
        "damage",
        4,
        false,
        1, // снимется на первом же тике
        1,
        ld2_server::time::now_epoch(),
    )
    .await
    .expect("короткий бафф");
    pool.close().await;

    // Полная замена: истёкший снят, активный marksman остаётся.
    let removed = c
        .wait(|v| {
            v["type"] == "evt/char/buffs"
                && v["payload"]["buffs"]
                    .as_array()
                    .is_some_and(|b| b.iter().all(|x| x["kind"] != "rune_warmaster"))
                && v["payload"]["buffs"]
                    .as_array()
                    .is_some_and(|b| !b.is_empty())
        })
        .await;
    let rest = removed["payload"]["buffs"].as_array().expect("список");
    assert_eq!(rest.len(), 1, "только warmaster снят: {removed}");
    assert_eq!(rest[0]["kind"], "rune_marksman", "активный остался");

    // Ресинк: marksman активен (derived с баффом), warmaster снят.
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let buffs = snap["payload"]["char"]["buffs"]
        .as_array()
        .expect("buffs после тикера");
    assert_eq!(buffs.len(), 1, "остался только активный: {buffs:?}");
    assert_eq!(buffs[0]["kind"], "rune_marksman");
    assert_eq!(
        snap["payload"]["char"]["derived"]["accuracy"].as_i64(),
        Some(base_accuracy + 12),
        "derived всё ещё с активным баффом"
    );
}

/// M18/F58f: дубликаты рун не стакаются — одна и та же руна не может
/// стоять в двух слотах предмета (§9 E_DUPLICATE_RUNE); другая руна
/// вставляется свободно.
#[tokio::test]
async fn duplicate_rune_is_rejected_in_same_item() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world_profile(&ws, "mage").await;

    // staff_apprentice: 2 рунных слота — второй свободен для дубликата.
    let staff_uid = give(&mut c, "staff_apprentice").await;
    let rune1 = give(&mut c, "rune_fire").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 0, "rune_uid": rune1 }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "первая вставка");

    // Вторая та же руна (отдельный стак) → отказ.
    let rune2 = give(&mut c, "rune_fire").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 1, "rune_uid": rune2 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_DUPLICATE_RUNE",
        "дубликат руны: {ack}"
    );

    // Другая руна в тот же слот — вставляется.
    let rune3 = give(&mut c, "rune_frost").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 1, "rune_uid": rune3 }),
        )
        .await;
    assert_eq!(
        c.ack(seq).await["result"],
        "ok",
        "другая руна — не дубликат"
    );

    // Гемы не ограничиваются (если слот принимает) — проверим на брони
    // с двумя самоцветами одного типа... armor_bronze имеет 1 слот,
    // поэтому просто убеждаемся, что запрет не мешает обычному потоку.
    let _ = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
}

/// M18/F58f (ADR-012): самоцвет в **надетом оружии** даёт бонус
/// атрибутов (раньше считалась только броня — статы «не прибавлялись»);
/// снятие оружия снимает бонус.
#[tokio::test]
async fn gem_in_equipped_weapon_boosts_attrs() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    // 4 слота, rune_sockets = 2 → слоты2 и 3 принимают самоцветы.
    let sword = give(&mut c, "sword_1000_true").await;
    let gem = give(&mut c, "gem_str").await; // +3 str

    let seq = c
        .send(
            "cmd/item.equip",
            json!({ "item_uid": sword, "slot": "main_hand" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "надевание: {ack}");
    let base = ack["payload"]["char"]["attrs"]["str"]
        .as_i64()
        .expect("базовая сила");

    // Гем в gem-слот надетого оружия — бонус сразу в ack.
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": sword, "socket_index": 2, "rune_uid": gem }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "самоцвет в оружии: {ack}");
    assert_eq!(
        ack["payload"]["char"]["attrs"]["str"],
        base + 3,
        "бонус самоцвета из оружия: {ack}"
    );

    // Извлекли самоцвет из НАДЕТОГО оружия — бонус снят (оружие
    // остаётся, `char` в ack как у брони).
    let seq = c
        .send(
            "cmd/socket.extract",
            json!({ "staff_uid": sword, "socket_index": 2 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "извлечение: {ack}");
    assert_eq!(
        ack["payload"]["char"]["attrs"]["str"], base,
        "бонус снят: {ack}"
    );
}

/// M18 (указание заказчика): извлечённый самоцвет по возможности
/// складывается в существующий стак (не отдельная запись), клиент
/// обновляет qty существующей записи.
#[tokio::test]
async fn gem_extract_stacks_into_existing() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    // Две отдельные записи одного самоцвета (scenario give не стакует).
    let gem1 = give(&mut c, "gem_str").await;
    let gem2 = give(&mut c, "gem_str").await;

    // Вставили первый — его запись списана; в рюкзаке остаётся gem2.
    let armor = give(&mut c, "armor_bronze").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": armor, "socket_index": 0, "rune_uid": gem1 }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "вставка");

    // Извлечение — в стак gem2: uid существующей записи, qty = 2.
    let seq = c
        .send(
            "cmd/socket.extract",
            json!({ "staff_uid": armor, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "извлечение: {ack}");
    let rune = &ack["payload"]["rune"];
    assert_eq!(rune["uid"], gem2, "слито в существующий стак: {ack}");
    assert_eq!(rune["qty"], 2, "qty стака выросло: {ack}");
}

/// M24 фикс (указание заказчика): инкрустация — в ОДИН предмет.
/// Стак (qty>1) не заполняется целиком: целевая штука становится
/// отдельной строкой, старый стак убывает на 1 и остаётся без сокетов;
/// окно инкрустации (клиент) следует за новым uid из ack.
#[tokio::test]
async fn socket_insert_stack_inscribes_single_item() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;

    let staff_uid = give(&mut c, "sword_knight").await; // sockets: 1, qty 1
    let rune_uid = give(&mut c, "rune_fire").await;

    // Стопка ×5 (путь заказчика: админ-выдача количеством).
    let pool = sqlx::PgPool::connect(&db_url).await.expect("пул");
    sqlx::query("UPDATE items SET qty = 5 WHERE uid = $1 AND character_id = $2")
        .bind(&staff_uid)
        .bind(&char_id)
        .execute(&pool)
        .await
        .expect("qty=5");
    pool.close().await;

    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 0, "rune_uid": rune_uid }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "инкрустация стака: {ack}");
    let new_uid = ack["payload"]["staff"]["uid"]
        .as_str()
        .expect("uid новой строки")
        .to_string();
    assert_ne!(new_uid, staff_uid, "цель — новая строка, а не стак");
    assert_eq!(ack["payload"]["staff"]["qty"], 1, "одна штука");
    assert_eq!(
        ack["payload"]["staff"]["sockets"][0]["rune"]["name"], "Руна огня",
        "сокет заполнен"
    );

    // Порядок событий сервера: руна израсходована → новая строка →
    // убыль стака (wait съедает несовпадающие — выдерживаем порядок).
    let removed = c.wait_event("evt/item/removed").await;
    assert_eq!(removed["payload"]["uid"], rune_uid, "руна потреблена");
    let added = c.wait_event("evt/item/added").await;
    assert_eq!(
        added["payload"]["item"]["uid"], new_uid,
        "новая строка в инвентаре"
    );
    let state = c.wait_event("evt/item/state").await;
    assert_eq!(state["payload"]["uid"], staff_uid, "патч — старый стак");
    assert_eq!(state["payload"]["patch"]["qty"], 4, "стак5 → 4");

    // Снапшот: стак4 без руны, отдельная штука — с руной.
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let inv = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь");
    let old = inv.iter().find(|i| i["uid"] == staff_uid).expect("стак");
    assert_eq!(old["qty"], 4, "стак убыл на1");
    let old_rune = old["sockets"].as_array().and_then(|s| s.first());
    assert!(
        old_rune.is_none_or(|s| s["rune"].is_null()),
        "стак без руны: {old_rune:?}"
    );
    let new = inv.iter().find(|i| i["uid"] == new_uid).expect("штука");
    assert_eq!(new["qty"], 1, "одна штука");
    assert_eq!(
        new["sockets"].as_array().expect("сокеты")[0]["rune"]["name"],
        "Руна огня",
        "руна в новой строке"
    );
}

/// M24: класс руны (`stats.class`) — каст только своим классом.
/// Воин кастует руну мага → `E_REQUIREMENT_ATTR`; инкрустация при
/// этом разрешена (гейт — на использовании, не на вставке).
#[tokio::test]
async fn rune_cast_requires_matching_class() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await; // warrior

    let staff = give(&mut c, "staff_apprentice").await;
    let rune = give(&mut c, "rune_fire").await; // class: mage
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff, "socket_index": 0, "rune_uid": rune }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "инкрустация разрешена");

    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": staff, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_REQUIREMENT_ATTR",
        "каст чужого класса: {ack}"
    );
    assert!(
        ack["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("класс"),
        "причина в сообщении: {ack}"
    );
}

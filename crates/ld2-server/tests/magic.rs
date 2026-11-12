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

    // Руна в броню — отказ.
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
            .contains("оружие"),
        "текст отказа: {ack}"
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

/// M17/F30a: меч принимает руны (инкрустация/извлечение, damage в
/// сокете), но каст руны остаётся привилегией посоха.
#[tokio::test]
async fn sword_takes_runes_but_cannot_cast() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    let sword = give(&mut c, "sword_knight").await; // sockets: 1 (контент)
    let rune = give(&mut c, "rune_fire").await;
    // M18/F31: сокеты видны в снапшоте до первой вставки — меч
    // попадает в список целей инкрустации (окно их отфильтровывает).
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "resync");
    let snap = c.wait_event("evt/state/snapshot").await;
    let item = snap["payload"]["char"]["inventory"]
        .as_array()
        .expect("инвентарь")
        .iter()
        .find(|i| i["uid"] == sword)
        .expect("меч в инвентаре")
        .clone();
    let socks = item["sockets"].as_array().expect("сокеты меча");
    assert_eq!(socks.len(), 1, "сокет до вставки: {item}");
    assert!(socks[0]["rune"].is_null(), "сокет свободен: {item}");

    // Инкрустация в меч — паритет с посохом; damage пишется в сокет.
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": sword, "socket_index": 0, "rune_uid": rune }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "инкрустация в меч: {ack}");
    assert_eq!(
        ack["payload"]["staff"]["sockets"][0]["rune"]["damage"], 12,
        "damage руны в сокете: {ack}"
    );
    let removed = c.wait_event("evt/item/removed").await;
    assert_eq!(removed["payload"]["uid"], rune, "руна израсходована");

    // Каст из меча — отказ: только посох (E_NO_RUNE_SLOT).
    let seq = c
        .send(
            "cmd/magic.rune_cast",
            json!({ "staff_uid": sword, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NO_RUNE_SLOT", "каст с меча: {ack}");

    // Извлечение обратно — руна возвращается в ack (без item/added:
    // клиент применяет её из payload, см. store::socket_ack).
    let seq = c
        .send(
            "cmd/socket.extract",
            json!({ "staff_uid": sword, "socket_index": 0 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "извлечение из меча: {ack}");
    assert_eq!(ack["payload"]["rune"]["name"], "Руна огня", "{ack}");
    assert_eq!(
        ack["payload"]["rune"]["qty"], 1,
        "руна вернулась (новый инстанс)"
    );
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

    // Первый каст: урон 9, волк жив (18 → 9).
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
        .wait(|v| {
            v["type"] == "evt/combat/hit"
                && v["payload"]["source"] == char
                && v["payload"]["amount"] == 9
        })
        .await;
    assert_eq!(hit["payload"]["target"], "m_3", "урон волку: {hit}");
    // M17/F30f: слова заклинания и «чем бьёт» — в строке кастера,
    // магические слова жирные.
    let text = hit["payload"]["text"].as_str().unwrap_or_default();
    assert!(text.contains("**Neris Vael**"), "слова жирные: {text}");
    assert!(text.contains("ледяной стрелой"), "чем бьёт: {text}");
    assert!(text.contains("вы бьёте"), "кастер-формат: {text}");
    let hp = c
        .wait(|v| v["type"] == "evt/entity/hp" && v["payload"]["entity_id"] == "m_3")
        .await;
    assert_eq!(hp["payload"]["hp"]["cur"], 9, "18 − 9: {hp}");

    // Второй каст добивает: труп + XP в школу `frost`; M17/F30g:
    // кулдаун свитка (1 с) между кастами одного определения.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let seq = c
        .send(
            "cmd/magic.scroll_cast",
            json!({ "item_uid": scroll2, "target_id": "m_3" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "каст-добивание");
    let corpse = c.wait_event("evt/corpse/created").await;
    assert!(corpse["payload"]["entity"]["id"].is_string(), "id трупа");
    let end = c.wait_event("evt/combat/end").await;
    assert_eq!(end["payload"]["reason"], "цель уничтожена");
    let xp = c.wait_event("evt/char/xp").await;
    assert_eq!(xp["payload"]["skill_id"], "frost", "школа заклинания");
    assert_eq!(xp["payload"]["amount"], 26, "8 + difficulty(3) × 6");

    // Дальше свиток кончился → E_ITEM_GONE.
    let seq = c
        .send(
            "cmd/magic.scroll_cast",
            json!({ "item_uid": scroll2, "target_id": "m_3" }),
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
    // Полный бар за 6 с → реген ~10 маны/с (без боя). rune_cost = 30:
    // два каста обнуляют бар, а для набора 30 маны регену нужно 3 с —
    // «пустой бар» детерминирован даже на медленном CI (гонка мс vs 3 с).
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.magic.mana.regen_out_s = 6;
            cfg.gameplay.magic.mana.rune_cost = 30;
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

    // 2 каста × 30 маны; M17/F30g: между кастами кулдаун руны (1 с),
    // пауза — окно регена (вне боя +10/с при regen_out_s=6), поэтому
    // точное mp_left второго каста зависит от фазы реген-тика.
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
            // Списано 30 — бар ниже цены каста (гейт пустого бара
            // покрыт в rune_cast_requires_mana).
            assert!(
                cast["payload"]["mp_left"].as_i64().unwrap_or(60) < 30,
                "бар ниже цены каста: {cast}"
            );
        }
    }

    // Реген по тику (§2.4): бар поднимается до цены каста (~3 с при 10/с).
    let stats = c
        .wait(|v| {
            v["type"] == "evt/char/stats"
                && v["payload"]["patch"]["mp"]["cur"].as_i64().unwrap_or(0) >= 30
        })
        .await;
    let mp = stats["payload"]["patch"]["mp"]["cur"].as_i64().expect("mp");
    assert!(mp >= 30, "реген поднял бар хотя бы до цены каста: {mp}");

    // Кулдаун руны (M17/F30g) давно истёк — каст после регена проходит.
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

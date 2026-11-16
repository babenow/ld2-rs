//! Улучшение предметов (ФР-10, M12.5c): превью → старт, золото,
//! `E_UPGRADE_PREVIEW_STALE`/`E_UPGRADE_MAX`, правила провала
//! (`features."upgrade.rules"`).

mod common;

use common::in_world;
use ld2_server::config::FeatureValue;
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

/// M18/F54: выдать камень усиления и выбрать его (`item.use`) —
/// возвращает (uid, качество в % пунктах). Ответ подтверждает выбор.
async fn use_stone(c: &mut ld2_test_kit::WsClient, def_id: &str) -> (String, i64) {
    let uid = give(c, def_id).await;
    let seq = c.send("cmd/item.use", json!({ "item_uid": uid })).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "выбор камня {def_id}: {ack}");
    assert_eq!(ack["payload"]["stone_uid"], uid, "ack несёт uid камня");
    let quality = ack["payload"]["quality"].as_i64().expect("качество");
    (uid, quality)
}

/// Начислить золото dev-сценарием.
async fn give_gold(c: &mut ld2_test_kit::WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "gold {amount}: {ack}");
}

#[tokio::test]
async fn upgrade_preview_start_success() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Шанс 1.0 → бросок всегда успешен (upgrade_roll ∈ [0,1)).
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.upgrade.chance_base = 1.0;
            cfg.gameplay.upgrade.chance_per_level = 0.0;
        })
        .await
    );
    let (mut c, _login, _char) = in_world(&ws).await;

    let uid = give(&mut c, "dagger_knife").await;
    give_gold(&mut c, 1000).await;
    use_stone(&mut c, "stone_upgrade_medium").await;
    // M18/F54: без камня превью недоступно — средний (нейтральный).
    use_stone(&mut c, "stone_upgrade_medium").await;

    // Превью: delta «+1», стоимость 100 (gold_base), шанс 1.0.
    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "превью: {ack}");
    let preview_id = ack["payload"]["preview_id"]
        .as_str()
        .expect("preview_id")
        .to_string();
    assert_eq!(ack["payload"]["delta"]["upgrade"], "+1", "дельта");
    assert_eq!(ack["payload"]["cost"]["gold"], 100, "gold_base");
    assert!((ack["payload"]["chance"].as_f64().unwrap() - 1.0).abs() < f64::EPSILON);

    // Старт: успех → result + патч upgrade.
    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": uid, "preview_id": preview_id }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "старт: {ack}");
    let result = c.wait_event("evt/upgrade/result").await;
    assert_eq!(result["payload"]["uid"], uid);
    assert_eq!(result["payload"]["ok"], true, "успех: {result}");
    assert_eq!(result["payload"]["new_upgrade"], 1);
    assert_eq!(result["payload"]["cost"]["gold"], 100, "золото списано");
    let patch = c.wait_event("evt/item/state").await;
    assert_eq!(patch["payload"]["uid"], uid);
    assert_eq!(patch["payload"]["patch"]["upgrade"], 1, "патч: {patch}");

    // Повторный старт с тем же preview_id → stale (одноразовое превью).
    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": uid, "preview_id": preview_id }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_UPGRADE_PREVIEW_STALE",
        "повтор: {ack}"
    );
}

#[tokio::test]
async fn upgrade_requires_fresh_preview_and_gold() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    let uid = give(&mut c, "dagger_knife").await;
    // M18/F54: превью требует выбранный камень.
    use_stone(&mut c, "stone_upgrade_medium").await;

    // Старт без превью → stale.
    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": uid, "preview_id": "pv_nope" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_UPGRADE_PREVIEW_STALE",
        "без превью: {ack}"
    );

    // Превью есть, но чужой uid → stale.
    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let preview_id = ack["payload"]["preview_id"].as_str().unwrap().to_string();
    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": "i_someone_else", "preview_id": preview_id }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_UPGRADE_PREVIEW_STALE",
        "чужой uid: {ack}"
    );

    // Нет золота → E_INGREDIENTS (превью уже израсходовано — новое).
    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let preview_id = ack["payload"]["preview_id"].as_str().unwrap().to_string();
    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": uid, "preview_id": preview_id }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_INGREDIENTS", "нет золота: {ack}");
}

#[tokio::test]
async fn upgrade_max_level_rejected() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Максимум +1: после одного успеха превью отказывает.
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.upgrade.max_level = 1;
            cfg.gameplay.upgrade.chance_base = 1.0;
            cfg.gameplay.upgrade.chance_per_level = 0.0;
        })
        .await
    );
    let (mut c, _login, _char) = in_world(&ws).await;

    let uid = give(&mut c, "dagger_knife").await;
    give_gold(&mut c, 1000).await;
    use_stone(&mut c, "stone_upgrade_medium").await;

    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let preview_id = ack["payload"]["preview_id"].as_str().unwrap().to_string();
    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": uid, "preview_id": preview_id }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    c.wait_event("evt/upgrade/result").await;
    c.wait_event("evt/item/state").await;

    // Потолок достигнут → и превью, и старт с новым превью — E_UPGRADE_MAX.
    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_UPGRADE_MAX", "потолок: {ack}");
}

#[tokio::test]
async fn upgrade_fail_break_on_fail_destroys_item() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Шанс 0 → всегда провал; правило break_on_fail (из конфига шаблона).
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.upgrade.chance_base = 0.0;
            cfg.gameplay.upgrade.chance_per_level = 0.0;
            cfg.gameplay.features.insert(
                "upgrade.rules".into(),
                FeatureValue::Str("break_on_fail".into()),
            );
        })
        .await
    );
    let (mut c, _login, _char) = in_world(&ws).await;

    let uid = give(&mut c, "dagger_knife").await;
    give_gold(&mut c, 1000).await;
    use_stone(&mut c, "stone_upgrade_medium").await;

    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let preview_id = ack["payload"]["preview_id"].as_str().unwrap().to_string();
    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": uid, "preview_id": preview_id }),
        )
        .await;
    assert_eq!(
        c.ack(seq).await["result"],
        "ok",
        "запуск (провал — не ошибка)"
    );

    // Предмет разрушен: result с item_destroyed + удаление из инвентаря
    // (порядок §2.2: result идёт первым).
    let result = c.wait_event("evt/upgrade/result").await;
    assert_eq!(result["payload"]["ok"], false, "провал: {result}");
    assert_eq!(
        result["payload"]["item_destroyed"], true,
        "разрушен: {result}"
    );
    // Камень списан первым — ждём именно удаление предмета (uid).
    let removed = c
        .wait(|v| v["type"] == "evt/item/removed" && v["payload"]["uid"] == uid.as_str())
        .await;
    assert_eq!(removed["payload"]["uid"], uid, "удалён: {removed}");

    // Дальше превью по этому uid → предмета нет.
    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_ITEM_GONE", "после гибели: {ack}");
}

#[tokio::test]
async fn upgrade_fail_keep_rule_preserves_item() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.upgrade.chance_base = 0.0;
            cfg.gameplay.upgrade.chance_per_level = 0.0;
            cfg.gameplay
                .features
                .insert("upgrade.rules".into(), FeatureValue::Str("keep".into()));
        })
        .await
    );
    let (mut c, _login, _char) = in_world(&ws).await;

    let uid = give(&mut c, "dagger_knife").await;
    give_gold(&mut c, 1000).await;
    use_stone(&mut c, "stone_upgrade_medium").await;

    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok");
    let preview_id = ack["payload"]["preview_id"].as_str().unwrap().to_string();
    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": uid, "preview_id": preview_id }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");

    let result = c.wait_event("evt/upgrade/result").await;
    assert_eq!(result["payload"]["ok"], false, "провал: {result}");
    assert_ne!(
        result["payload"]["item_destroyed"], true,
        "keep — предмет цел: {result}"
    );
    assert_eq!(result["payload"]["new_upgrade"], 0, "уровень без изменений");

    // Предмет жив: повторное превью работает (M18/F54: камень первого
    // улучшения потрачен — нужен новый).
    use_stone(&mut c, "stone_upgrade_medium").await;
    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "предмет пережил провал: {ack}");
}

// ─── M18/F54: камни усиления ────────────────────────────────────────────────

/// Превью без выбранного камня — `E_INGREDIENTS`.
#[tokio::test]
async fn upgrade_preview_requires_stone() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    let uid = give(&mut c, "dagger_knife").await;
    give_gold(&mut c, 1000).await;

    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_INGREDIENTS",
        "без камня превью невозможно: {ack}"
    );
    assert!(
        ack["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("камень"),
        "сообщение называет причину: {ack}"
    );

    // Выбор камня снимает ошибку.
    use_stone(&mut c, "stone_upgrade_medium").await;
    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "с камнем превью есть");
}

/// ack выбора камня несёт uid и качество; качество `stats.quality`
/// малого/среднего/большого камня: −5 / 0 / +5.
#[tokio::test]
async fn stone_use_returns_quality() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    for (def_id, expected) in [
        ("stone_upgrade_small", -5),
        ("stone_upgrade_medium", 0),
        ("stone_upgrade_large", 5),
    ] {
        let (_, quality) = use_stone(&mut c, def_id).await;
        assert_eq!(quality, expected, "качество {def_id}");
    }
}

/// Камень не расходуется при выборе (`item.use`), но тратится 1 шт. на
/// улучшение — событие `evt/item/removed`.
#[tokio::test]
async fn stone_consumed_once_per_upgrade() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // Шанс 1.0 → успех гарантирован (нейтральный камень не двигает шанс).
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.upgrade.chance_base = 1.0;
            cfg.gameplay.upgrade.chance_per_level = 0.0;
        })
        .await
    );
    let (mut c, _login, _char) = in_world(&ws).await;

    let uid = give(&mut c, "dagger_knife").await;
    give_gold(&mut c, 1000).await;
    let (stone_uid, _) = use_stone(&mut c, "stone_upgrade_large").await;

    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "превью: {ack}");
    let preview_id = ack["payload"]["preview_id"].as_str().unwrap().to_string();
    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": uid, "preview_id": preview_id }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "старт: улучшение прошло");

    // События идут в порядке: ack → списание золота → удаление камня →
    // результат. Ждём камень первым — иначе wait(result) его съест.
    let removed = c
        .wait(|v| v["type"] == "evt/item/removed" && v["payload"]["uid"] == stone_uid.as_str())
        .await;
    assert_eq!(
        removed["payload"]["reason"], "consumed",
        "камень списан: {removed}"
    );
    c.wait_event("evt/upgrade/result").await;

    // Камня больше нет → следующее превью снова просит камень.
    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_INGREDIENTS",
        "после расхода камня нужен новый"
    );
}

/// Качество камня реально двигает шанс: ±5% на нулевом уровне и
/// затухание 1/(1+level) проверяются на ответе превью.
#[tokio::test]
async fn stone_quality_moves_chance() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    // База 0.8, без понижения за уровень — чтобы разница читалась.
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.gameplay.upgrade.chance_base = 0.8;
            cfg.gameplay.upgrade.chance_per_level = 0.0;
        })
        .await
    );
    let (mut c, _login, _char) = in_world(&ws).await;

    let uid = give(&mut c, "dagger_knife").await;
    give_gold(&mut c, 1000).await;

    // Выбрать камень и вернуть шанс из превью по этому предмету.
    async fn chance_with(c: &mut ld2_test_kit::WsClient, item: &str, stone: &str) -> f64 {
        use_stone(c, stone).await;
        let seq = c
            .send("cmd/upgrade.preview", json!({ "item_uid": item }))
            .await;
        let ack = c.ack(seq).await;
        assert_eq!(ack["result"], "ok", "превью: {ack}");
        ack["payload"]["chance"].as_f64().expect("шанс")
    }

    let small = chance_with(&mut c, &uid, "stone_upgrade_small").await;
    let medium = chance_with(&mut c, &uid, "stone_upgrade_medium").await;
    let large = chance_with(&mut c, &uid, "stone_upgrade_large").await;

    assert!((small - 0.75).abs() < 1e-9, "малый −5%: {small}");
    assert!(
        (medium - 0.8).abs() < 1e-9,
        "средний без изменений: {medium}"
    );
    assert!((large - 0.85).abs() < 1e-9, "большой +5%: {large}");
    // Разброс ровно 5% в обе стороны от нейтрального.
    assert!(((large - medium) - 0.05).abs() < 1e-9, "верхняя граница");
    assert!(((medium - small) - 0.05).abs() < 1e-9, "нижняя граница");
}

/// Рейты сервера (`gameplay.rates.chance`) — множитель любого шанса:
/// 0.0 обнуляет даже базу 1.0, 2.0 уводит шанс в потолок 1.0.
#[tokio::test]
async fn server_rates_scale_chance() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    for (rate, expected) in [(0.0, 0.0), (0.5, 0.5), (2.0, 1.0)] {
        let ws = format!(
            "ws://{}/ws",
            common::start_server_with(&db_url, |cfg| {
                cfg.gameplay.upgrade.chance_base = 1.0;
                cfg.gameplay.upgrade.chance_per_level = 0.0;
                cfg.gameplay.rates.chance = rate;
            })
            .await
        );
        let (mut c, _login, _char) = in_world(&ws).await;
        let uid = give(&mut c, "dagger_knife").await;
        give_gold(&mut c, 1000).await;
        use_stone(&mut c, "stone_upgrade_medium").await;

        let seq = c
            .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
            .await;
        let ack = c.ack(seq).await;
        assert_eq!(ack["result"], "ok", "превью: {ack}");
        let chance = ack["payload"]["chance"].as_f64().expect("шанс");
        assert!(
            (chance - expected).abs() < 1e-9,
            "рейт {rate} → шанс {chance}, ожидалось {expected}"
        );
        c.close().await;
    }
}

/// Между превью и стартом активный камень может смениться — сервер
/// обязан отвергнуть старт (`E_INGREDIENTS`), а не бросать по чужому.
#[tokio::test]
async fn start_rejects_swapped_stone() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    let uid = give(&mut c, "dagger_knife").await;
    give_gold(&mut c, 1000).await;
    use_stone(&mut c, "stone_upgrade_medium").await;

    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "превью: {ack}");
    let preview_id = ack["payload"]["preview_id"].as_str().unwrap().to_string();

    // Игрок выбрал другой камень уже после превью.
    use_stone(&mut c, "stone_upgrade_large").await;

    let seq = c
        .send(
            "cmd/upgrade.start",
            json!({ "item_uid": uid, "preview_id": preview_id }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_INGREDIENTS",
        "смена камня после превью: {ack}"
    );
}

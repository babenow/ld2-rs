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
    let removed = c.wait_event("evt/item/removed").await;
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

    // Предмет жив: повторное превью работает.
    let seq = c
        .send("cmd/upgrade.preview", json!({ "item_uid": uid }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "предмет пережил провал: {ack}");
}

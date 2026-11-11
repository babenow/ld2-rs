//! M16/F29b: `/scenario kit` — тестовый набор рюкзака: предметы
//! разных типов и редкостей, улучшения +N, полная экипировка и
//! стак монет.

mod common;

use common::in_world;
use serde_json::json;

#[tokio::test]
async fn scenario_kit_fills_backpack_and_equip() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char_id) = in_world(&ws).await;

    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": "/scenario kit" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "команда принята");

    // Снапшот после выдачи (события item/added отбрасываются wait'ом).
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "resync");
    let snap = c.wait_event("evt/state/snapshot").await;
    let ch = &snap["payload"]["char"];
    let inv = ch["inventory"].as_array().expect("инвентарь");

    // Монеты: стак 500 (F29a).
    let money: i64 = inv
        .iter()
        .filter(|i| i["type"] == "money")
        .map(|i| i["qty"].as_i64().unwrap_or(0))
        .sum();
    assert_eq!(money, 500, "стак монет: {inv:?}");

    // Редкости и улучшения: меч rare +3, сапоги rare +5, доспех uncommon +4.
    assert!(
        inv.iter()
            .any(|i| i["name"] == "Рыцарский меч" && i["rarity"] == "rare" && i["upgrade"] == 3),
        "меч: {inv:?}"
    );
    assert!(
        inv.iter()
            .any(|i| i["name"] == "Сапоги странника" && i["upgrade"] == 5),
        "сапоги: {inv:?}"
    );
    assert!(
        inv.iter().any(|i| i["name"] == "Бронзовый доспех"
            && i["rarity"] == "uncommon"
            && i["upgrade"] == 4),
        "доспех: {inv:?}"
    );

    // Типы разные: броня, расходники, материалы, инструменты.
    for item_type in ["armor", "potion", "rune", "herb", "quill"] {
        assert!(
            inv.iter().any(|i| i["type"] == item_type),
            "нет типа {item_type}: {inv:?}"
        );
    }

    // Экипировка: все 5 слотов конфига заняты.
    let equip = ch["equip"].as_object().expect("equip");
    let filled = equip.values().filter(|v| !v.is_null()).count();
    assert_eq!(filled, 5, "все слоты экипированы: {equip:?}");
    // Слоты указывают на реальные предметы инвентаря.
    let uids: Vec<&str> = inv.iter().filter_map(|i| i["uid"].as_str()).collect();
    for (slot, uid) in equip {
        let uid = uid.as_str().expect("uid в слоте");
        assert!(uids.contains(&uid), "слот {slot} → {uid} есть в инвентаре");
    }
}

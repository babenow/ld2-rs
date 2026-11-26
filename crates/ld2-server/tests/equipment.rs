//! M17/F30b: правила экипировки — слот↔предмет, руки (двуручник
//! блокирует левую ячейку), классы (воин/лучник/маг), парные слоты.

mod common;

use common::in_world_profile;
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

async fn equip(c: &mut ld2_test_kit::WsClient, uid: &str, slot: &str) -> serde_json::Value {
    let seq = c
        .send("cmd/item.equip", json!({ "item_uid": uid, "slot": slot }))
        .await;
    c.ack(seq).await
}

fn err_msg(ack: &serde_json::Value) -> String {
    assert_eq!(ack["result"], "error", "ожидалась ошибка: {ack}");
    ack["error"]["message"]
        .as_str()
        .expect("message")
        .to_string()
}

#[tokio::test]
async fn warrior_shield_and_sword_or_twohander() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world_profile(&ws, "warrior").await;

    // Посох воину закрыт.
    let staff = give(&mut c, "staff_apprentice").await;
    let ack = equip(&mut c, &staff, "main_hand").await;
    assert!(err_msg(&ack).contains("воин не носит посох"), "{ack}");

    // Меч + щит.
    let sword = give(&mut c, "sword_knight").await;
    let ack = equip(&mut c, &sword, "main_hand").await;
    assert_eq!(ack["result"], "ok", "меч: {ack}");
    let shield = give(&mut c, "shield_wood").await;
    let ack = equip(&mut c, &shield, "off_hand").await;
    assert_eq!(ack["result"], "ok", "щит: {ack}");
    assert_eq!(ack["payload"]["char"]["equip"]["off_hand"], shield);

    // Двуручник занимает обе руки — щит снимается (ячейка блокируется).
    let twohand = give(&mut c, "twohand_sword").await;
    let ack = equip(&mut c, &twohand, "main_hand").await;
    assert_eq!(ack["result"], "ok", "двуручник: {ack}");
    assert_eq!(ack["payload"]["char"]["equip"]["main_hand"], twohand);
    assert!(
        ack["payload"]["char"]["equip"]["off_hand"].is_null(),
        "левая рука заблокирована: {ack}"
    );

    // С двуручником щит не надеть; после снятия — можно.
    let ack = equip(&mut c, &shield, "off_hand").await;
    assert!(
        err_msg(&ack).contains("левая рука занята двуручным"),
        "{ack}"
    );
    let seq = c
        .send("cmd/item.unequip", json!({ "slot": "main_hand" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "снятие двуручника");
    let ack = equip(&mut c, &shield, "off_hand").await;
    assert_eq!(ack["result"], "ok", "щит после снятия: {ack}");

    // Лук воину закрыт; щит — не в правую руку (щит сейчас надет —
    // снимаем, чтобы дошла проверка слот↔предмет, а не «уже надет»).
    let bow = give(&mut c, "bow_hunter").await;
    let ack = equip(&mut c, &bow, "main_hand").await;
    assert!(err_msg(&ack).contains("воин не носит луки"), "{ack}");
    let seq = c
        .send("cmd/item.unequip", json!({ "slot": "off_hand" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "снятие щита");
    let ack = equip(&mut c, &shield, "main_hand").await;
    assert!(err_msg(&ack).contains("не надевается в этот слот"), "{ack}");
}

#[tokio::test]
async fn mage_only_staff_archer_only_bows() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;

    // Маг: только посох.
    let (mut c, _login, _char) = in_world_profile(&ws, "mage").await;
    let sword = give(&mut c, "sword_knight").await;
    let ack = equip(&mut c, &sword, "main_hand").await;
    assert!(err_msg(&ack).contains("маг носит только посох"), "{ack}");
    let staff = give(&mut c, "staff_apprentice").await;
    let ack = equip(&mut c, &staff, "main_hand").await;
    assert_eq!(ack["result"], "ok", "посох мага: {ack}");
    let shield = give(&mut c, "shield_iron").await;
    let ack = equip(&mut c, &shield, "off_hand").await;
    assert!(err_msg(&ack).contains("маг носит только посох"), "{ack}");

    // Лучник: только луки.
    let (mut c, _login, _char) = in_world_profile(&ws, "archer").await;
    let bow = give(&mut c, "bow_long").await;
    let ack = equip(&mut c, &bow, "main_hand").await;
    assert_eq!(ack["result"], "ok", "лук лучника: {ack}");
    let dagger = give(&mut c, "dagger_knife").await;
    let ack = equip(&mut c, &dagger, "main_hand").await;
    assert!(err_msg(&ack).contains("лучник носит только луки"), "{ack}");
}

/// M17/F30e: ресурс по профилю в снапшоте (WoW): воин — ярость с
/// нуля (максимум 100), лучник — полная энергия (100).
#[tokio::test]
async fn resource_bar_by_profile() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;

    let (mut c, _login, _char) = in_world_profile(&ws, "warrior").await;
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "resync воина");
    let snap = c.wait_event("evt/state/snapshot").await;
    let mp = &snap["payload"]["char"]["mp"];
    assert_eq!(mp["max"], 100, "максимум ярости из конфига: {mp}");
    assert_eq!(mp["cur"], 0, "ярость стартует с нуля: {mp}");

    let (mut c, _login, _char) = in_world_profile(&ws, "archer").await;
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "resync лучника");
    let snap = c.wait_event("evt/state/snapshot").await;
    let mp = &snap["payload"]["char"]["mp"];
    assert_eq!(mp["max"], 100, "максимум энергии: {mp}");
    assert_eq!(mp["cur"], 100, "энергия полная на старте: {mp}");
}

#[tokio::test]
async fn paired_slots_and_slot_match() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world_profile(&ws, "warrior").await;

    // Кольца занимают пару ring1/ring2 по очереди.
    let ring1 = give(&mut c, "ring_silver").await;
    let ack = equip(&mut c, &ring1, "ring1").await;
    assert_eq!(ack["result"], "ok", "кольцо 1: {ack}");
    let ring2 = give(&mut c, "ring_ruby").await;
    let ack = equip(&mut c, &ring2, "ring2").await;
    assert_eq!(ack["result"], "ok", "кольцо 2: {ack}");

    // Кольцо не надевается в слот шлема.
    let ack = equip(&mut c, &ring2, "head").await;
    // ring2 уже надето → раньше слота сработает «предмет уже надет»;
    // снимаем и проверяем слот↔предмет на свободном предмете.
    if ack["result"] == "error" {
        let seq = c.send("cmd/item.unequip", json!({ "slot": "ring2" })).await;
        assert_eq!(c.ack(seq).await["result"], "ok");
    }
    let ack = equip(&mut c, &ring2, "head").await;
    assert!(err_msg(&ack).contains("не надевается в этот слот"), "{ack}");

    // Расходник вовсе не экипируется.
    let potion = give(&mut c, "potion_heal").await;
    let ack = equip(&mut c, &potion, "main_hand").await;
    assert!(err_msg(&ack).contains("не экипируется"), "{ack}");
}

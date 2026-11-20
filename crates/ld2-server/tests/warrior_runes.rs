//! M25: руны воина (класс warrior) — авто-приём с меча. Отдельный
//! бинарь: единственный тест → сид временного контента без гонок
//! (паттерн tests/rune_effects.rs).

mod common;

use common::in_world_profile;
use serde_json::json;
use std::path::PathBuf;

/// Временный контент: копия каталога + шанс1.0 у руны ярости.
fn temp_content() -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let tmp = std::env::temp_dir().join(format!(
        "ld2-wrune-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("время")
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).expect("temp content");
    for entry in std::fs::read_dir(&src).expect("content dir") {
        let entry = entry.expect("entry");
        std::fs::copy(entry.path(), tmp.join(entry.file_name())).expect("copy");
    }
    let items_path = tmp.join("items.json");
    let mut items: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&items_path).expect("items.json"))
            .expect("parse items");
    let fury = items["items"]
        .as_array_mut()
        .expect("items")
        .iter_mut()
        .find(|i| i["id"] == "rune_fury")
        .expect("rune_fury");
    fury["stats"]["chance"] = json!(1.0); // гарантированное срабатывание
    std::fs::write(
        &items_path,
        serde_json::to_string_pretty(&items).expect("ser"),
    )
    .expect("write items");
    tmp
}

/// M25: руны воина работают — авто-приём с меча (класс warrior,
/// шанс1.0): заряд тратится при первом ударе.
#[tokio::test]
async fn warrior_sword_rune_auto_procs() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let tmp = temp_content();
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.world.content_dir = tmp.clone();
        })
        .await
    );
    let (mut c, _login, _char) = in_world_profile(&ws, "warrior").await;

    let sword = give(&mut c, "sword_knight").await;
    let rune = give(&mut c, "rune_fury").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": sword, "socket_index": 0, "rune_uid": rune }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "инкрустация");
    let seq = c
        .send(
            "cmd/item.equip",
            json!({ "item_uid": sword, "slot": "main_hand" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "экипировка");

    // Удар по волку → авто-приём (шанс1.0) → расход заряда патчем.
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
            v["type"] == "evt/item/state"
                && v["payload"]["uid"] == sword
                && v["payload"]["patch"]["sockets"].is_array()
        })
        .await;
    let charges = patch["payload"]["patch"]["sockets"][0]["rune"]["charges"]
        .as_i64()
        .expect("charges в патче");
    assert!(
        charges < 6,
        "заряд потрачен авто-приёмом (6 → {charges}): {patch}"
    );
}

/// Выдать предмет dev-командой → uid.
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

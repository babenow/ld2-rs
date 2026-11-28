//! M38: горное дело — жилы, опыт, камни и ювелирная обработка.

mod common;

use common::in_world;
use ld2_test_kit::WsClient;
use serde_json::json;
use std::path::PathBuf;

/// Временный контент: лесная медная жила с гарантированным камнем,
/// оловянная — с высоким уровнем для проверки гейта.
fn temp_content() -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let tmp = std::env::temp_dir().join(format!(
        "ld2-mining-{}-{}",
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
    let path = tmp.join("locations.json");
    let mut locs: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("locations")).expect("parse");
    for loc in locs["locations"].as_array_mut().expect("locations") {
        if loc["id"] != "l_forest" {
            continue;
        }
        for entity in loc["entities"].as_array_mut().expect("entities") {
            match entity["id"].as_str() {
                Some("r_forest_ore") => {
                    entity["required_level"] = json!(1);
                    entity["gems"] = json!({
                        "chance": 1.0,
                        "mixed_share": 0.0,
                        "mixed": [],
                        "pure": ["raw_stone_armor"]
                    });
                }
                Some("r_forest_tin") => {
                    entity["required_level"] = json!(30);
                    entity["min_rank"] = json!(1);
                }
                _ => {}
            }
        }
    }
    std::fs::write(&path, serde_json::to_string_pretty(&locs).expect("ser"))
        .expect("write locations");
    tmp
}

/// M39: бронзовая жила — ранг 2, уровень 75, в «Железных штольнях».
#[test]
fn bronze_vein_content_is_configured() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let items: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(src.join("items.json")).expect("items"))
            .expect("items json");
    let ore = items["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["id"] == "ore_bronze")
        .expect("ore_bronze");
    assert_eq!(ore["type"], "ore_bronze");
    assert_eq!(ore["name"], "Бронзовая руда");

    let locs: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(src.join("locations.json")).expect("locations"),
    )
    .expect("locations json");
    let iron = locs["locations"]
        .as_array()
        .expect("locations")
        .iter()
        .find(|l| l["id"] == "l_mine_iron")
        .expect("l_mine_iron");
    let bronze = iron["entities"]
        .as_array()
        .expect("entities")
        .iter()
        .find(|e| e["id"] == "ore_bronze_1")
        .expect("ore_bronze_1");
    assert_eq!(bronze["resource"], "ore_bronze");
    assert_eq!(bronze["required_level"], 75);
    assert_eq!(bronze["min_rank"], 2);
}

async fn give(c: &mut WsClient, def_id: &str) -> String {
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

async fn give_gold(c: &mut WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
}

async fn learn(c: &mut WsClient, id: &str) {
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": id }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "learn {id}: {ack}");
    c.wait_event("evt/craft/profession").await;
}

/// M39: волк в лесу агрессивен — урон прерывает сбор; убиваем до добычи.
async fn kill_wolf(c: &mut WsClient) {
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "атака волка");
    c.wait_event("evt/corpse/created").await;
    c.wait_event("evt/combat/end").await;
}

#[tokio::test]
async fn mining_drops_gem_and_jeweler_cuts_it() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let tmp = temp_content();
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.world.content_dir = tmp;
            // M39: тест проверяет лут/гемы, а не шанс добычи.
            cfg.gameplay.professions.mining_chance_base = 1.0;
            cfg.gameplay.professions.mining_chance_min = 1.0;
        })
        .await
    );
    let (mut c, _login, _char) = in_world(&ws).await;
    give_gold(&mut c, 1500).await;
    learn(&mut c, "mining").await;
    learn(&mut c, "jeweler").await;
    give(&mut c, "pickaxe").await;
    give(&mut c, "tool_chisel").await;

    // В лес: медная жила даёт руду и гарантированный камень.
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    kill_wolf(&mut c).await;
    let seq = c
        .send(
            "cmd/resource.gather",
            json!({ "entity_id": "r_forest_ore" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "медь");
    let ore = c
        .wait(|v| v["type"] == "evt/item/added" && v["payload"]["item"]["def_id"] == "ore_copper")
        .await;
    assert_eq!(ore["payload"]["item"]["qty"], 1);
    let gem = c
        .wait(|v| {
            v["type"] == "evt/item/added" && v["payload"]["item"]["def_id"] == "raw_stone_armor"
        })
        .await;
    let raw_uid = gem["payload"]["item"]["uid"].as_str().expect("uid");
    assert_eq!(gem["payload"]["item"]["type"], "raw_stone_armor");

    // M39: опыт горного дела обратен шансу (chance=1.0 в тесте → 10 XP).
    let prof = c.wait_event("evt/craft/profession").await;
    assert_eq!(prof["payload"]["profession"]["id"], "mining");
    assert_eq!(prof["payload"]["gain"], 10, "XP меди: {prof}");

    // Гейт уровня: оловянная жила ур.30 при уровне 0 → E_LEVEL_REQUIRED.
    let seq = c
        .send(
            "cmd/resource.gather",
            json!({ "entity_id": "r_forest_tin" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_LEVEL_REQUIRED", "уровень: {ack}");

    // В деревню: ювелирный стол, резец и камень → гранёный сардоникс.
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_south" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в деревню");
    let seq = c
        .send(
            "cmd/craft.start",
            json!({ "recipe_id": "rec_cut_stone_armor_1", "qty": 1 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "старт огранки: {ack}");
    let result = c
        .wait(|v| {
            v["type"] == "evt/craft/result" && v["payload"]["recipe_id"] == "rec_cut_stone_armor_1"
        })
        .await;
    assert_eq!(result["payload"]["ok"], true, "огранка: {result}");
    let made = result["payload"]["made"].as_array().expect("made");
    let gem_uid = made[0]["uid"].as_str().expect("uid").to_string();

    // Камень брони в сокет брони: +2 armor к 6 → 8.
    let armor_uid = give(&mut c, "armor_bronze").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": armor_uid, "socket_index": 0, "rune_uid": gem_uid }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "вставка камня");
    let seq = c
        .send(
            "cmd/item.equip",
            json!({ "item_uid": armor_uid, "slot": "body" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "надеть броню: {ack}");
    assert_eq!(
        ack["payload"]["char"]["derived"]["armor"], 8,
        "броня 6 + камень 2: {ack}"
    );
    let _ = raw_uid;
}

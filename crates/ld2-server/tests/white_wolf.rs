//! M37: Белый волк (элита) — три приёма (укус с кровотечением,
//! громкий вой с призывом двух волков, рык с баффом урона) и снятие
//! шкуры с 45-го уровня + редкая шкура (3 %).

mod common;

use common::in_world;
use serde_json::json;
use std::path::PathBuf;

/// Временный контент: копия каталога + быстрый тестовый белый волк в
/// лесу (кулдауны 1 с, урон 0 — персонаж переживает проверки).
fn temp_content() -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let tmp = std::env::temp_dir().join(format!(
        "ld2-white-wolf-{}-{}",
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
    let loc_path = tmp.join("locations.json");
    let mut locs: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&loc_path).expect("locations.json"))
            .expect("parse locations");
    for loc in locs["locations"].as_array_mut().expect("locations") {
        if loc["id"] != "l_forest" {
            continue;
        }
        for entity in loc["entities"].as_array_mut().expect("entities") {
            // Обычный волк не должен мешать проверкам уроном (его же
            // клонирует вой).
            if entity["id"] == "m_3" {
                entity["damage"] = json!(0);
            }
        }
        loc["entities"]
            .as_array_mut()
            .expect("entities")
            .push(json!({
                "id": "m_test_white_wolf",
                "kind": "monster",
                "name": "Белый волк",
                "hp": { "cur": 200, "max": 200 },
                "tags": ["aggro", "elite"],
                "aggro": true,
                "difficulty": 6,
                "damage": 0,
                "boss_ai": {
                    "phases": [
                        { "after_s": 0, "name": "Белый волк", "glyph": "W" }
                    ],
                    "skills": [
                        { "id": "bite", "name": "Укус", "glyph": "b",
                          "cooldown_s": 1, "damage": 0,
                          "effect": { "kind": "bleed", "duration_s": 2, "value": 1 } },
                        { "id": "howl", "name": "Громкий вой", "glyph": "h",
                          "cooldown_s": 1, "damage": 0,
                          "summon": { "mob": "m_3", "count": 2 } },
                        { "id": "growl", "name": "Рык", "glyph": "g",
                          "cooldown_s": 1, "damage": 0,
                          "self_buff": { "damage_mult": 1.5, "interval_mult": 1.0,
                                         "duration_s": 10 } }
                    ]
                }
            }));
    }
    std::fs::write(&loc_path, serde_json::to_string_pretty(&locs).expect("ser"))
        .expect("write locations");
    tmp
}

/// Контент: Белый волк — элита в Чёрном лесу, шкура с 45 уровня и
/// редкая шкура 3 %; три приёма на месте.
#[test]
fn white_wolf_content_skinning_and_skills() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let locs: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("locations.json")).expect("locs"))
            .expect("parse");
    let forest = locs["locations"]
        .as_array()
        .expect("locations")
        .iter()
        .find(|l| l["id"] == "l_forest")
        .expect("Чёрный лес");
    let wolf = forest["entities"]
        .as_array()
        .expect("entities")
        .iter()
        .find(|e| e["id"] == "m_white_wolf")
        .expect("белый волк в лесу");
    assert_eq!(wolf["name"], "Белый волк");
    assert!(
        wolf["tags"]
            .as_array()
            .expect("tags")
            .iter()
            .any(|t| t == "elite"),
        "элита: {wolf}"
    );
    assert_eq!(wolf["harvest"]["level"], 45, "снятие шкур с 45 уровня");
    assert_eq!(wolf["harvest"]["skin"], true);
    let extra = &wolf["harvest"]["skin_extra"][0];
    assert_eq!(extra["def_id"], "pelt_white_wolf");
    assert_eq!(extra["chance"], 0.03, "шкура белого волка — 3 %");

    let names: Vec<&str> = wolf["boss_ai"]["skills"]
        .as_array()
        .expect("skills")
        .iter()
        .map(|s| s["name"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(names, ["Укус", "Громкий вой", "Рык"], "три приёма");

    let items: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("items.json")).expect("items"))
            .expect("parse");
    let pelt = items["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["id"] == "pelt_white_wolf")
        .expect("шкура белого волка в контенте");
    assert_eq!(pelt["name"], "Шкура белого волка");
}

/// Приёмы: укус вешает кровотечение (HP падает без обычного урона),
/// вой призывает двух волков, рык бафает волка.
#[tokio::test]
async fn white_wolf_bite_howl_growl() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let tmp = temp_content();
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.world.content_dir = tmp.clone();
            cfg.gameplay.combat.attack_interval_s = 0.1;
        })
        .await
    );
    let (mut c, _login, _char_id) = in_world(&ws).await;

    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    // Белый волк не агрится первым — атакуем, чтобы он ответил приёмами.
    let seq = c
        .send(
            "cmd/combat.attack",
            json!({ "target_id": "m_test_white_wolf" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "атака белого волка");

    // 1) Укус — кровотечение на персонаже.
    let afflict = c
        .wait(|v| v["type"] == "evt/char/afflict" && v["payload"]["kind"] == "bleed")
        .await;
    assert_eq!(afflict["payload"]["title"], "Кровотечение", "{afflict}");

    // 2) Громкий вой — навык и два призванных волка.
    let howl = c
        .wait(|v| {
            v["type"] == "evt/mob/skill"
                && v["payload"]["entity_id"] == "m_test_white_wolf"
                && v["payload"]["name"] == "Громкий вой"
        })
        .await;
    assert_eq!(howl["payload"]["source_name"], "Белый волк", "{howl}");
    for i in 0..2 {
        let spawn = c
            .wait(|v| {
                v["type"] == "evt/entity/spawn"
                    && v["payload"]["entity"]["id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("m_3_summon_"))
            })
            .await;
        assert_eq!(spawn["payload"]["entity"]["name"], "Волк", "волк #{i}");
        assert_eq!(spawn["payload"]["location_id"], "l_forest");
    }

    // 3) Кровотечение тикает: HP падает (обычные атаки тестового волка — 0).
    let stats = c
        .wait(|v| {
            v["type"] == "evt/char/stats"
                && v["payload"]["patch"]["hp"]["cur"]
                    .as_i64()
                    .is_some_and(|hp| hp < 100)
        })
        .await;
    let hp = stats["payload"]["patch"]["hp"]["cur"].as_i64().expect("hp");
    assert!(hp < 100, "кровотечение сняло HP: {stats}");

    // 4) Рык — бафф на себя (навык применён).
    let growl = c
        .wait(|v| {
            v["type"] == "evt/mob/skill"
                && v["payload"]["entity_id"] == "m_test_white_wolf"
                && v["payload"]["name"] == "Рык"
        })
        .await;
    assert_eq!(growl["payload"]["source_name"], "Белый волк", "{growl}");
}

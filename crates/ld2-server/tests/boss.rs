//! M25: босс-дракон против реального сервера (гейт:
//! LD2_TEST_DATABASE_URL): фазы по времени боя, навыки на кулдауне,
//! бар атаки монстров и минимальное время атаки на сервере.

mod common;

use common::in_world;
use serde_json::json;
use std::path::PathBuf;

/// Временный контент: копия каталога + тест-босс в лесу (фазы через
/// 1 с, навык с кулдауном 1 с) — паттерн tests/rune_effects.rs.
fn temp_content() -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let tmp = std::env::temp_dir().join(format!(
        "ld2-boss-{}-{}",
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
        if loc["id"] == "l_forest" {
            loc["entities"]
                .as_array_mut()
                .expect("entities")
                .push(json!({
                    "id": "m_test_boss",
                    "kind": "monster",
                    "name": "Испытательный дракон",
                    "hp": { "cur": 30, "max": 30 },
                    "tags": ["boss", "aggro"],
                    "aggro": true,
                    "difficulty": 5,
                    "damage": 1,
                    "boss_ai": {
                        "phases": [
                            { "after_s": 0, "name": "Пробуждение", "glyph": "a",
                              "damage_mult": 1.0, "interval_mult": 1.0 },
                            { "after_s": 1, "name": "Ярость", "glyph": "b",
                              "damage_mult": 1.5, "interval_mult": 0.8 }
                        ],
                        "skills": [
                            { "id": "test_bolt", "name": "Испытательный разряд",
                              "glyph": "x", "cooldown_s": 1, "damage": 1 }
                        ]
                    }
                }));
        }
    }
    std::fs::write(&loc_path, serde_json::to_string_pretty(&locs).expect("ser"))
        .expect("write locations");
    tmp
}

/// M25: босс — фазы по времени боя (событие с глифом), навык на
/// кулдауне, бар атаки в снапшоте и пол минимального времени атаки
/// (конфиг 0.1 с → сервер держит 500 мс).
#[tokio::test]
async fn boss_phases_skills_and_attack_bar() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let tmp = temp_content();
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.world.content_dir = tmp.clone();
            // Конфиг ниже пола — сервер обязан держать минимум 500 мс.
            cfg.gameplay.combat.attack_interval_s = 0.1;
        })
        .await
    );
    let (mut c, _login, _char) = in_world(&ws).await;

    // В лес — босс агрессивен, первая атака сразу при входе.
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");

    // 1) Бар атаки: событие с интервалом = пол 500 мс.
    let attack = c
        .wait(|v| v["type"] == "evt/mob/attack" && v["payload"]["entity_id"] == "m_test_boss")
        .await;
    assert_eq!(
        attack["payload"]["interval_ms"], 500,
        "минимальное время атаки: {attack}"
    );
    assert!(
        attack["payload"]["next_attack_at"].as_i64().unwrap_or(0) > 0,
        "срок следующей атаки: {attack}"
    );

    // 2) Фаза по времени боя (after_s = 1) — событие с глифом.
    let phase = c
        .wait(|v| {
            v["type"] == "evt/mob/phase"
                && v["payload"]["entity_id"] == "m_test_boss"
                && v["payload"]["phase"].as_u64().unwrap_or(0) >= 1
        })
        .await;
    assert_eq!(phase["payload"]["name"], "Ярость", "имя фазы: {phase}");
    assert_eq!(phase["payload"]["glyph"], "b", "глиф фазы: {phase}");

    // 3) Навык босса (кулдаун 1 с) — событие для журнала.
    let skill = c
        .wait(|v| v["type"] == "evt/mob/skill" && v["payload"]["entity_id"] == "m_test_boss")
        .await;
    assert_eq!(
        skill["payload"]["name"], "Испытательный разряд",
        "навык: {skill}"
    );
    assert_eq!(
        skill["payload"]["source_name"], "Испытательный дракон",
        "источник: {skill}"
    );

    // 4) Снапшот: босс-состояние и параметры бара у монстра.
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    let snap = c.wait_event("evt/state/snapshot").await;
    let boss = snap["payload"]["location"]["entities"]
        .as_array()
        .expect("сущности")
        .iter()
        .find(|e| e["id"] == "m_test_boss")
        .expect("босс в снапшоте")
        .clone();
    assert!(
        boss["boss"]["phase"].as_u64().unwrap_or(0) >= 1,
        "фаза в снапшоте: {boss}"
    );
    assert_eq!(boss["attack_interval"], 500, "интервал: {boss}");
    assert!(
        boss["next_attack_at"].as_i64().unwrap_or(0) > 0,
        "бар: {boss}"
    );
}

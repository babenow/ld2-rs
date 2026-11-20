//! M25: босс-дракон против реального сервера (гейт:
//! LD2_TEST_DATABASE_URL): фазы по времени боя, каст навыков
//! (подготовка — босс не атакует), навыки на кулдауне, бар атаки
//! монстров, цель атаки и минимальное время атаки на сервере.

mod common;

use common::in_world;
use serde_json::json;
use std::path::PathBuf;

/// Временный контент: копия каталога + тест-босс в лесу — фазы через
/// 1 с, навык с кастом 1 с/кулдауном 5 с, замедление атаки +500 мс
/// (паттерн tests/rune_effects.rs).
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
                    "attack_speed": 500,
                    "boss_ai": {
                        "phases": [
                            { "after_s": 0, "name": "Пробуждение", "glyph": "a",
                              "damage_mult": 1.0, "interval_mult": 1.0 },
                            { "after_s": 1, "name": "Ярость", "glyph": "b",
                              "damage_mult": 1.5, "interval_mult": 0.8 }
                        ],
                        "skills": [
                            { "id": "test_bolt", "name": "Испытательный разряд",
                              "glyph": "x", "cooldown_s": 5, "damage": 1,
                              "cast_s": 1,
                              "effect": { "kind": "stun", "duration_s": 2 } }
                        ]
                    }
                }));
        }
    }
    std::fs::write(&loc_path, serde_json::to_string_pretty(&locs).expect("ser"))
        .expect("write locations");
    tmp
}

/// M25: босс — каст (подготовка), фазы по времени боя (глиф), навык
/// после каста, бар атаки/цель в снапшоте; интервал = (конфиг0.1 с =
/// 100 мс + замедление500 мс) × множитель фазы, пол500 мс:
/// фаза1 = 600, фаза2 = 480 → пол500.
#[tokio::test]
async fn boss_cast_phases_skills_and_attack_bar() {
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
    let (mut c, _login, char_id) = in_world(&ws).await;

    // В лес — босс агрессивен, каст начинается при входе.
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");

    // 1) Каст (подготовка) — событие до применения.
    let cast = c
        .wait(|v| v["type"] == "evt/mob/cast" && v["payload"]["entity_id"] == "m_test_boss")
        .await;
    assert_eq!(cast["payload"]["name"], "Испытательный разряд", "{cast}");
    assert!(
        cast["payload"]["ends_at"].as_i64().unwrap_or(0) > 0,
        "срок каста: {cast}"
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

    // 3) Навык применён после каста (кулдаун5 с).
    let skill = c
        .wait(|v| v["type"] == "evt/mob/skill" && v["payload"]["entity_id"] == "m_test_boss")
        .await;
    assert_eq!(
        skill["payload"]["name"], "Испытательный разряд",
        "навык: {skill}"
    );

    // 4) Эффект приёма — стан: событие и блок действий (E_STUNNED).
    let afflict = c
        .wait(|v| v["type"] == "evt/char/afflict" && v["payload"]["kind"] == "stun")
        .await;
    assert_eq!(afflict["payload"]["title"], "Оглушение", "{afflict}");
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_south" }))
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_STUNNED",
        "стан блокирует действия"
    );
    // Стан истёк (2 с) — действия снова доступны.
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_south" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "после стана можно");
    // Вернуться в лес — продолжаем проверки бара/снапшота.
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "обратно в лес");

    // 5) Обычная атака после применения — бар атаки.
    let attack = c
        .wait(|v| v["type"] == "evt/mob/attack" && v["payload"]["entity_id"] == "m_test_boss")
        .await;
    assert_eq!(
        attack["payload"]["interval_ms"], 500,
        "фаза2: (конфиг100 + замедление500)×0.8=480 → пол500: {attack}"
    );
    assert!(
        attack["payload"]["next_attack_at"].as_i64().unwrap_or(0) > 0,
        "срок следующей атаки: {attack}"
    );

    // 5) Снапшот: фаза, бар, цель атаки.
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
    assert_eq!(
        boss["attack_interval"], 500,
        "фаза2: (100+500)×0.8=480 → пол500: {boss}"
    );
    assert!(
        boss["next_attack_at"].as_i64().unwrap_or(0) > 0,
        "бар: {boss}"
    );
    assert_eq!(
        boss["attacking"], char_id,
        "монстр атакует персонажа: {boss}"
    );
}

//! M18/F58d (ADR-012 §5): авто-приёмы рун — шанс/урон/бафф и эффекты
//! на мобах (кровотечение/стан/корни).
//!
//! Отдельный бинарь с **временным контентом** (chance/buff/effect у
//! rune_fire): тестовые бинари cargo запускает последовательно, а внутри
//! бинара оба теста используют одинаковый контент → сид в общую БД
//! идемпотентен и не конфликтует с основными тестами (гонка сидов
//! `item_defs` была причиной флейков при параллельном запуске).

mod common;

use common::in_world_profile;
use ld2_test_kit::WsClient;
use serde_json::json;
use std::path::PathBuf;

/// Выдача предмета через dev-команду (как в magic.rs).
async fn give(c: &mut WsClient, def_id: &str) -> String {
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

/// Временный каталог контента: rune_fire получает шанс 1.0, бафф
/// точности и кровотечение; волку поднят HP, чтобы не пал от одного
/// удара (DoT должен успеть тикнуть). Оба теста бинара используют
/// **идентичный** контент — сиды параллельных тестов безвредны.
fn temp_content() -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let tmp = std::env::temp_dir().join(format!(
        "ld2-rune-effects-{}-{}",
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
    let rune = items["items"]
        .as_array_mut()
        .expect("items")
        .iter_mut()
        .find(|i| i["id"] == "rune_fire")
        .expect("rune_fire");
    rune["stats"]["chance"] = json!(1.0); // гарантированное срабатывание
    rune["stats"]["buff"] =
        json!({ "param": "accuracy", "value": 5, "duration_s": 60, "max_stacks": 3 });
    rune["stats"]["effect"] = json!({ "kind": "bleed", "value": 5, "duration_s": 10 });
    std::fs::write(
        &items_path,
        serde_json::to_string_pretty(&items).expect("ser"),
    )
    .expect("write items");
    // Волк (18 hp) пал бы от базы 6 + руны 12 за первый удар — DoT не
    // успел бы тикнуть; поднимаем HP в временной копии контента.
    let loc_path = tmp.join("locations.json");
    let mut locs: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&loc_path).expect("locations.json"))
            .expect("parse locations");
    for loc in locs["locations"].as_array_mut().expect("locations") {
        for e in loc["entities"]
            .as_array_mut()
            .map(Vec::as_mut)
            .unwrap_or(&mut Vec::new())
        {
            if e["id"] == "m_3" {
                e["hp"] = json!({ "cur": 40, "max": 40 });
            }
        }
    }
    std::fs::write(&loc_path, serde_json::to_string_pretty(&locs).expect("ser"))
        .expect("write locations");
    tmp
}

/// Экипировать посох с руной (кэш рун собирается при экипировке).
async fn equip_rune_staff(c: &mut WsClient) -> String {
    let staff_uid = give(c, "staff_apprentice").await;
    let rune_uid = give(c, "rune_fire").await;
    let seq = c
        .send(
            "cmd/socket.insert",
            json!({ "staff_uid": staff_uid, "socket_index": 0, "rune_uid": rune_uid }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "инкрустация");
    let seq = c
        .send(
            "cmd/item.equip",
            json!({ "item_uid": staff_uid, "slot": "main_hand" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "экипировка");
    staff_uid
}

/// M18/F58d: авто-приём — урон по контенту, бафф наложен, заряды
/// израсходованы (БД+патч), кэш синхронизирован со вторым ударом.
#[tokio::test]
async fn rune_auto_proc_fires_damage_spends_and_buffs() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let tmp = temp_content();
    let ws = format!(
        "ws://{}/ws",
        common::start_server_with(&db_url, |cfg| {
            cfg.world.content_dir = tmp.clone();
            cfg.gameplay.magic.mana.rune_cost = 1;
        })
        .await
    );
    let (mut c, _login, _char) = in_world_profile(&ws, "mage").await;

    let staff_uid = equip_rune_staff(&mut c).await;

    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "переход");
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "атака");

    // Один проход: рунный hit + патч сокетов + бафф-замена (порядок в
    // пределах тика не гарантирован → собираем всё вместе); хвост —
    // второй удар (заряды 5) либо конец боя.
    let mut rune_hit: Option<serde_json::Value> = None;
    let mut buffs: Option<serde_json::Value> = None;
    let mut patch: Option<serde_json::Value> = None;
    let mut patch2: Option<serde_json::Value> = None;
    let mut after: Option<serde_json::Value> = None;
    while rune_hit.is_none()
        || buffs.is_none()
        || patch.is_none()
        || (patch2.is_none() && after.is_none())
    {
        let v = c.recv_json().await;
        match v["type"].as_str() {
            Some("evt/combat/hit") if rune_hit.is_none() && v["payload"]["kind"] == "rune" => {
                rune_hit = Some(v)
            }
            Some("evt/char/buffs")
                if buffs.is_none()
                    && !v["payload"]["buffs"]
                        .as_array()
                        .is_none_or(|a| a.is_empty()) =>
            {
                buffs = Some(v)
            }
            Some("evt/item/state")
                if patch.is_none()
                    && v["payload"]["uid"] == staff_uid.as_str()
                    && v["payload"]["patch"]["sockets"].is_array() =>
            {
                patch = Some(v)
            }
            // Второй удар (заряды 5) — либо волк (40 hp) пережил бы
            // первый… но от 6+12×2 он падает к 3-му удару; хватает
            // обоих исходов.
            Some("evt/item/state")
                if patch.is_some()
                    && patch2.is_none()
                    && v["payload"]["uid"] == staff_uid.as_str()
                    && v["payload"]["patch"]["sockets"]
                        .as_array()
                        .is_some_and(|ss| ss[0]["rune"]["charges"].as_i64() == Some(5)) =>
            {
                patch2 = Some(v.clone())
            }
            Some("evt/combat/end") | Some("evt/corpse/created") if after.is_none() => {
                after = Some(v.clone())
            }
            _ => {}
        }
    }

    // Рунный урон из контента (12).
    let hit = rune_hit.expect("рунный hit");
    assert_eq!(
        hit["payload"]["amount"].as_i64(),
        Some(12),
        "урон руны огня из контента: {hit}"
    );

    // Бафф наложен → полная замена списка.
    let buffs = buffs.expect("evt/char/buffs");
    let list = buffs["payload"]["buffs"].as_array().expect("buffs");
    assert_eq!(list.len(), 1, "бафф наложен: {buffs}");
    assert_eq!(list[0]["kind"], "rune_fire", "ключ — def_id руны");
    assert_eq!(list[0]["title"], "Руна огня");
    assert_eq!(list[0]["stacks"], 1);
    assert!(
        list[0]["expires_at"].as_i64().unwrap_or(0) > ld2_server::time::now_epoch(),
        "срок в будущем: {buffs}"
    );

    // Патч сокетов: заряды израсходованы (7 → 6).
    let patch = patch.expect("патч сокетов");
    assert_eq!(
        patch["payload"]["patch"]["sockets"][0]["rune"]["charges"].as_i64(),
        Some(6),
        "заряды потрачены авто-приёмом: {patch}"
    );

    if let Some(p2) = patch2 {
        assert_eq!(
            p2["payload"]["patch"]["sockets"][0]["rune"]["charges"].as_i64(),
            Some(5),
            "второй удар потратил заряд из синхронизированного кэша"
        );
    } else {
        let end = after.expect("2-й удар либо конец боя");
        assert!(
            matches!(
                end["type"].as_str(),
                Some("evt/combat/end") | Some("evt/corpse/created")
            ),
            "конец боя: {end}"
        );
    }

    let _ = c.send("cmd/combat.stop", json!({})).await;
    drop(c);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// M18/F58d (ADR-012 §5): эффект руны на моба — кровотечение (DoT)
/// тикает раз в секунду событием в локацию и может добить.
#[tokio::test]
async fn rune_bleed_effect_ticks_on_mob() {
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
    let (mut c, _login, _char) = in_world_profile(&ws, "mage").await;

    let _staff_uid = equip_rune_staff(&mut c).await;

    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "переход");
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "атака");

    // Первый DoT-тик — через секунду после срабатывания; волк жив
    // (40 hp), удары по 18 не добивают мгновенно.
    let dot = c
        .wait(|v| {
            v["type"] == "evt/combat/hit"
                && v["payload"]["target"] == "m_3"
                && v["payload"]["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("кровоточит"))
        })
        .await;
    assert_eq!(
        dot["payload"]["amount"].as_i64(),
        Some(5),
        "dps кровотечения из контента: {dot}"
    );

    let _ = c.send("cmd/combat.stop", json!({})).await;
    drop(c);
    let _ = std::fs::remove_dir_all(&tmp);
}

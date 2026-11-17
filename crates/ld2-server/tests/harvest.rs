//! M22: снятие шкур против реального сервера (гейт:
//! LD2_TEST_DATABASE_URL): гейты (профессия/нож), сервер-джоб5 с,
//! лут — шкуры/обрезки в общий список и мясо/потроха в блок «Внутри»,
//! XP только за снятие шкуры, повтор запрещён.

mod common;

use common::in_world;
use serde_json::json;

/// Выдать предмет dev-сценарием → uid.
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
        .expect("uid")
        .to_string()
}

/// Начислить золото.
async fn give_gold(c: &mut ld2_test_kit::WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
}

/// Старт разделки → ack.
async fn harvest(c: &mut ld2_test_kit::WsClient, corpse: &str, action: &str) -> serde_json::Value {
    let seq = c
        .send(
            "cmd/corpse.harvest",
            json!({ "entity_id": corpse, "action": action }),
        )
        .await;
    c.ack(seq).await
}

/// Итог разделки (сервер-джоб5 с — ждём дольше лимита wait в5 с).
async fn harvest_result(c: &mut ld2_test_kit::WsClient) -> serde_json::Value {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let v = c.recv_json().await;
            if v["type"] == "evt/harvest/result" {
                return v;
            }
        }
    })
    .await
    .expect("evt/harvest/result")
}

/// Убить волка в лесу → id трупа.
async fn kill_wolf(c: &mut ld2_test_kit::WsClient) -> String {
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    let seq = c
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "атака");
    let corpse = c.wait_event("evt/corpse/created").await;
    let id = corpse["payload"]["entity"]["id"]
        .as_str()
        .expect("id трупа")
        .to_string();
    // Бой окончен (цель уничтожена) — не мешает дальнейшим wait.
    c.wait_event("evt/combat/end").await;
    id
}

/// M22: полный флоу — гейты (нет профессии → нет ножа) → освежевание
/// (блок «Внутри») → снятие шкуры (XP/уровень) → повтор запрещён.
#[tokio::test]
async fn harvest_gates_gut_and_skin_flow() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
    let _ = char_id;

    let corpse = kill_wolf(&mut c).await;

    // Гейт1: профессия не изучена.
    let ack = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(ack["error"]["code"], "E_PROFESSION", "без профессии: {ack}");

    // Обучение — у тренера в деревне (труп ждёт: TTL60 с).
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_south" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в деревню");
    give_gold(&mut c, 1000).await;
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "skinning" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "learn skinning");
    c.wait_event("evt/craft/profession").await;
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "назад в лес");

    // Гейт2: ножа нет.
    let ack = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(ack["error"]["code"], "E_TOOL_MISSING", "без ножа: {ack}");

    give(&mut c, "skinning_knife").await;

    // Освежевание: старт → итог через5 с → блок «Внутри».
    let ack = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(ack["result"], "ok", "старт gut: {ack}");
    assert_eq!(ack["payload"]["action"], "gut");
    assert_eq!(ack["payload"]["name"], "Волк", "имя без «Труп:»");
    // Пока джоб идёт — второй старт запрещён.
    let ack2 = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(
        ack2["error"]["code"], "E_COOLDOWN",
        "повтор во время джоба: {ack2}"
    );

    let result = harvest_result(&mut c).await;
    assert_eq!(result["payload"]["ok"], true, "gut готов: {result}");
    assert_eq!(result["payload"]["action"], "gut");
    let info = {
        let seq = c
            .send("cmd/entity.info", json!({ "entity_id": corpse }))
            .await;
        c.ack(seq).await
    };
    let inner = info["payload"]["inner"].as_array().expect("inner");
    assert_eq!(inner.len(), 1, "мясо/потроха отдельно: {info}");
    let inner_def = inner[0]["def_id"].as_str().unwrap_or_default();
    assert!(
        inner_def == "meat" || inner_def == "offal",
        "нутри: {inner_def}"
    );
    // Повторное освежевание запрещено.
    let ack = harvest(&mut c, &corpse, "gut").await;
    assert_eq!(ack["error"]["code"], "E_ITEM_GONE", "повтор gut: {ack}");

    // Снятие шкуры: старт → итог + XP (треб волка5, уровень0 →
    // diff −5 →×1 →100 XP → уровень ≥2).
    let ack = harvest(&mut c, &corpse, "skin").await;
    assert_eq!(ack["result"], "ok", "старт skin: {ack}");
    let result = harvest_result(&mut c).await;
    assert_eq!(result["payload"]["ok"], true, "skin готов: {result}");
    let prof = c.wait_event("evt/craft/profession").await;
    assert_eq!(prof["payload"]["profession"]["id"], "skinning");
    let level = prof["payload"]["profession"]["level"].as_i64().unwrap_or(0);
    // M22: не больше1 уровня за начисление (600 XP → уровень1,
    // шкала зажата новым порогом).
    assert_eq!(level, 1, "ровно1 уровень за действие: {prof}");

    // Лут: дроп волка (2) + шкура/обрезки (1…3 с критом) — общий
    // список; «Внутри» не тронут.
    let info = {
        let seq = c
            .send("cmd/entity.info", json!({ "entity_id": corpse }))
            .await;
        c.ack(seq).await
    };
    let loot = info["payload"]["loot"].as_array().expect("loot");
    assert!(
        (3..=5).contains(&loot.len()),
        "дроп2 + шкура/обрезки (+крит×2): {loot:?}"
    );
    let tier = loot
        .iter()
        .skip(2)
        .map(|i| i["def_id"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(
        tier.iter()
            .any(|d| d.starts_with("hide_q") || d.starts_with("hide_scraps_q")),
        "качество по рангу (q1): {tier:?}"
    );
    assert_eq!(
        info["payload"]["inner"].as_array().expect("inner").len(),
        1,
        "inner не тронут"
    );

    // Повторное снятие шкуры запрещено.
    let ack = harvest(&mut c, &corpse, "skin").await;
    assert_eq!(ack["error"]["code"], "E_ITEM_GONE", "повтор skin: {ack}");

    // Снять с другого трупа без ножа нельзя — нож уже есть; проверим
    // запрет разделки без флагов: статический труп Кима (cor_9).
    let ack = harvest(&mut c, "cor_9", "skin").await;
    assert_eq!(ack["error"]["code"], "E_TARGET_GONE", "нет флагов: {ack}");
}

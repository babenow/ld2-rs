//! M18/F35: диалоги NPC против реального сервера (гейт:
//! LD2_TEST_DATABASE_URL): реплики с условиями (предмет/пол), тайминг
//! `every_s`, связь с квестами (взятый квест скрывает просьбу), отказы
//! для не-NPC/несуществующих сущностей.

mod common;

use common::in_world;
use ld2_test_kit::WsClient;
use serde_json::{json, Value};

fn unique_name(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("время монотонно")
        .as_nanos();
    format!("{prefix}_{nanos:x}")
}

/// `cmd/npc.talk` → ack.
async fn talk(c: &mut WsClient, npc_id: &str) -> Value {
    let seq = c.send("cmd/npc.talk", json!({ "entity_id": npc_id })).await;
    c.ack(seq).await
}

/// Идентификаторы видимых строк в ответе.
fn line_ids(ack: &Value) -> Vec<String> {
    ack["payload"]["dialog"]["lines"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|l| l["id"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Выдать предмет dev-сценарием (как в magic.rs).
async fn give(c: &mut WsClient, def_id: &str) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario give {def_id}") }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "give {def_id}: {ack}");
    c.wait_event("evt/item/added").await;
}

/// Регистрация свежего аккаунта (клиент готов к `char.*`).
async fn registered(ws: &str) -> (WsClient, String) {
    let login = ld2_test_kit::checks::unique_login();
    let mut c = WsClient::connect(ws).await;
    let seq = c
        .send(
            "cmd/auth.register",
            json!({
                "login": login,
                "password": "longenough1",
                "email": ld2_test_kit::checks::unique_email()
            }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok");
    c.wait_event("evt/session.start").await;
    (c, login)
}

/// M18/F35: базовая реплика всегда, условия (предмет/пол) открывают
/// свои строки; квест NPC — в списке, взятый квест скрывает просьбу и
/// исчезает из списка; несуществующая сущность → отказ.
#[tokio::test]
async fn npc_talk_lines_conditions_and_quests() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await; // мужчина, l_village

    // Первый диалог: рендер `{name}`, набор строк, квест в списке.
    let ack = talk(&mut c, "npc_elder").await;
    assert_eq!(ack["result"], "ok", "диалог: {ack}");
    let greeting = ack["payload"]["dialog"]["greeting"]
        .as_str()
        .unwrap_or_default();
    assert!(!greeting.contains('{'), "шаблон отрендерен: {greeting}");
    for line in ack["payload"]["dialog"]["lines"]
        .as_array()
        .expect("строки диалога")
    {
        assert!(
            !line["text"].as_str().unwrap_or_default().contains('{'),
            "текст строки отрендерен: {line}"
        );
    }
    let ids = line_ids(&ack);
    assert!(ids.contains(&"l_hello".into()), "базовая реплика: {ids:?}");
    assert!(!ids.contains(&"l_female".into()), "мужчине не про женщин");
    assert!(!ids.contains(&"l_pack".into()), "зелья ещё нет");
    assert!(
        ids.contains(&"l_errand".into()),
        "просьба-квест видна: {ids:?}"
    );
    let quests = ack["payload"]["quests"].as_array().expect("quests");
    assert!(
        quests.iter().any(|q| q["id"] == "q_wolf_hunt"),
        "квест NPC в списке: {quests:?}"
    );

    // Предметная реплика: зелье в рюкзаке → строка появляется.
    give(&mut c, "potion_heal").await;
    let ids = line_ids(&talk(&mut c, "npc_elder").await);
    assert!(ids.contains(&"l_pack".into()), "после зелья: {ids:?}");

    // Принимаем квест из диалога → просьба и квест исчезают.
    let seq = c
        .send("cmd/quest.accept", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "принятие квеста");
    let ack = talk(&mut c, "npc_elder").await;
    let ids = line_ids(&ack);
    assert!(
        !ids.contains(&"l_errand".into()),
        "взятый квест скрыл просьбу: {ids:?}"
    );
    let quests = ack["payload"]["quests"].as_array().expect("quests");
    assert!(
        quests.iter().all(|q| q["id"] != "q_wolf_hunt"),
        "взятого квеста нет в списке: {quests:?}"
    );

    // Несуществующая сущность → E_TARGET_GONE.
    let seq = c.send("cmd/npc.talk", json!({ "entity_id": "nope" })).await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_TARGET_GONE");
}

/// M18/F35: объект в деревне — не NPC (отказ); тайминг-реплика
/// (`every_s`) видна один раз за период — сразу второй диалог её не
/// показывает.
#[tokio::test]
async fn npc_talk_rejects_non_npc_and_times_lines() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    // Станция алхимии (object) в деревне — не NPC.
    let ack = talk(&mut c, "st_alchemy").await;
    assert_eq!(ack["error"]["code"], "E_TARGET_GONE", "объект: {ack}");
    assert!(
        ack["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("NPC"),
        "текст отказа: {ack}"
    );

    // Первый диалог показывает просьбу (every_s=1800) и метит её.
    let ids = line_ids(&talk(&mut c, "npc_elder").await);
    assert!(ids.contains(&"l_errand".into()), "первый показ: {ids:?}");
    // Сразу второй — в пределах окна её нет.
    let ids = line_ids(&talk(&mut c, "npc_elder").await);
    assert!(
        !ids.contains(&"l_errand".into()),
        "второй показ в окне скрыт: {ids:?}"
    );
}

/// M18/F35 + F34: персонаж-женщина видит гендерную реплику.
#[tokio::test]
async fn npc_talk_shows_gender_line_for_female_char() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login) = registered(&ws).await;

    let seq = c
        .send(
            "cmd/char.create",
            json!({
                "name": unique_name("Мира"),
                "profile": "mage",
                "gender": "female"
            }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "создание: {ack}");
    let char_id = ack["payload"]["character"]["id"]
        .as_str()
        .expect("id")
        .to_string();
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "выбор");
    c.wait_event("evt/state/snapshot").await;

    let ack = talk(&mut c, "npc_elder").await;
    assert_eq!(ack["result"], "ok", "диалог: {ack}");
    let ids = line_ids(&ack);
    assert!(ids.contains(&"l_female".into()), "женская реплика: {ids:?}");
    assert!(ids.contains(&"l_hello".into()), "базовая реплика: {ids:?}");
}

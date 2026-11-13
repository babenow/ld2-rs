//! M18/F35/F37: диалоги NPC против реального сервера (гейт:
//! LD2_TEST_DATABASE_URL): ветки реплик (персонаж отвечает — NPC
//! связывает строки), условия на строках/ответах, тайминг `every_s`,
//! связь с квестами, отказы не-NPC/несущих сущностей.

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

/// Ответ персонажа → ack.
async fn reply(c: &mut WsClient, npc_id: &str, node_id: &str, reply_id: &str) -> Value {
    let seq = c
        .send(
            "cmd/npc.reply",
            json!({ "entity_id": npc_id, "node_id": node_id, "reply_id": reply_id }),
        )
        .await;
    c.ack(seq).await
}

/// id текущего узла в ack.
fn node_id(ack: &Value) -> Option<&str> {
    ack["payload"]["node"]["id"].as_str()
}

/// id видимых ответов узла.
fn reply_ids(ack: &Value) -> Vec<String> {
    ack["payload"]["node"]["replies"]
        .as_array()
        .map(|replies| {
            replies
                .iter()
                .filter_map(|r| r["id"].as_str().map(String::from))
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

/// M18/F35/F37: корень ветки с ответами; ответ ведёт к узлу-просьбе
/// (квест + `every_s`); условия ответа (предмет) открывают ветку;
/// взятый квест скрывает просьбу; несущность → отказ.
#[tokio::test]
async fn npc_dialog_walks_branches_and_gates_by_quests() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await; // мужчина, l_village

    // Корень: рендер `{name}`, ответы с учётом условий.
    let ack = talk(&mut c, "npc_elder").await;
    assert_eq!(ack["result"], "ok", "диалог: {ack}");
    let greeting = ack["payload"]["dialog"]["greeting"]
        .as_str()
        .unwrap_or_default();
    assert!(
        !greeting.contains('{'),
        "приветствие отрендерено: {greeting}"
    );
    assert_eq!(node_id(&ack), Some("l_hello"), "корень ветки: {ack}");
    let text = ack["payload"]["node"]["text"].as_str().unwrap_or_default();
    assert!(!text.contains('{'), "текст узла отрендерен: {text}");
    let replies = reply_ids(&ack);
    assert!(
        replies.contains(&"r_work".into()),
        "просьба видна: {replies:?}"
    );
    assert!(
        replies.contains(&"r_bye".into()),
        "прощание видно: {replies:?}"
    );
    assert!(
        !replies.contains(&"r_pack".into()),
        "зелья нет → ответа нет"
    );
    assert!(!replies.contains(&"r_care".into()), "мужчине не про женщин");
    assert!(!replies.contains(&"r_glory".into()), "1-й уровень не воин");
    let quests = ack["payload"]["quests"].as_array().expect("quests");
    assert!(
        quests.iter().any(|q| q["id"] == "q_wolf_hunt"),
        "квест NPC в списке: {quests:?}"
    );

    // Ответ персонажа → узел-просьба (every_s метится при входе).
    let ack = reply(&mut c, "npc_elder", "l_hello", "r_work").await;
    assert_eq!(ack["result"], "ok", "ответ: {ack}");
    assert_eq!(node_id(&ack), Some("l_errand"), "переход: {ack}");
    let inner_text = ack["payload"]["node"]["text"].as_str().unwrap_or_default();
    assert!(!inner_text.contains('{'), "текст отрендерен: {inner_text}");
    assert!(
        reply_ids(&ack).contains(&"r_errand_bye".into()),
        "ответы нового узла: {ack}"
    );

    // Второй разговор сразу: просьба в окне `every_s` скрыта (метка).
    let replies = reply_ids(&talk(&mut c, "npc_elder").await);
    assert!(
        !replies.contains(&"r_work".into()),
        "второй показ в окне скрыт: {replies:?}"
    );

    // Предметная ветка: зелье → ответ r_pack появляется.
    give(&mut c, "potion_heal").await;
    let replies = reply_ids(&talk(&mut c, "npc_elder").await);
    assert!(
        replies.contains(&"r_pack".into()),
        "после зелья: {replies:?}"
    );

    // Принимаем квест → просьба-ветка исчезает, квеста в списке нет.
    let seq = c
        .send("cmd/quest.accept", json!({ "quest_id": "q_wolf_hunt" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "принятие квеста");
    let ack = talk(&mut c, "npc_elder").await;
    let replies = reply_ids(&ack);
    assert!(
        !replies.contains(&"r_work".into()),
        "взятый квест скрыл ветку: {replies:?}"
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

/// M18/F37: связки нет (чужая строка/ответ) → отказ; конец ветки
/// (`next: null`) → `{node: null}`; объект в деревне — не NPC.
#[tokio::test]
async fn npc_reply_validates_links_and_non_npc() {
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

    // Чужая строка / чужой ответ → отказы.
    let seq = c
        .send(
            "cmd/npc.reply",
            json!({ "entity_id": "npc_elder", "node_id": "l_nope", "reply_id": "r_bye" }),
        )
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_TARGET_GONE",
        "нет строки"
    );
    let seq = c
        .send(
            "cmd/npc.reply",
            json!({ "entity_id": "npc_elder", "node_id": "l_hello", "reply_id": "r_nope" }),
        )
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_TARGET_GONE",
        "нет ответа"
    );

    // Конец ветки: r_bye без next → node: null.
    let ack = reply(&mut c, "npc_elder", "l_hello", "r_bye").await;
    assert_eq!(ack["result"], "ok", "прощание: {ack}");
    assert!(ack["payload"]["node"].is_null(), "конец диалога: {ack}");
}

/// M18/F35/F37 + F34: персонаж-женщина видит ветку «берегись леса»
/// (условие на ответе/строке по полу).
#[tokio::test]
async fn npc_dialog_shows_gender_branch_for_female_char() {
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
    let replies = reply_ids(&ack);
    assert!(
        replies.contains(&"r_care".into()),
        "ветка для женщин видна: {replies:?}"
    );

    let ack = reply(&mut c, "npc_elder", "l_hello", "r_care").await;
    assert_eq!(ack["result"], "ok", "переход: {ack}");
    assert_eq!(node_id(&ack), Some("l_female"), "узел: {ack}");
    let text = ack["payload"]["node"]["text"].as_str().unwrap_or_default();
    assert!(!text.contains('{'), "текст отрендерен: {text}");
}

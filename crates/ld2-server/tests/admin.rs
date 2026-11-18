//! M24: админ-панель предметов против реального сервера (гейт:
//! LD2_TEST_DATABASE_URL): роль-гейты `E_PERMISSION`, каталог,
//! выдача/удаление через execute-диалог (qty 1..=999 → `E_BAD_QTY`),
//! режим бога, гейт `/scenario`.

mod common;

use common::in_world;
use ld2_test_kit::WsClient;
use serde_json::json;

/// Сделать аккаунт админом: роль читается при выдаче сессии — значит
/// до **повторного** входа.
async fn make_admin(db_url: &str, login: &str) {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::connect(&cfg.database).await.expect("БД");
    let n = sqlx::query("UPDATE accounts SET role = 'admin' WHERE login = $1")
        .bind(login)
        .execute(&pool)
        .await
        .expect("UPDATE role")
        .rows_affected();
    assert_eq!(n, 1, "аккаунт найден");
    pool.close().await;
}

/// Мир + админ: повторный вход перечитывает роль из БД.
async fn in_world_admin(ws: &str, db_url: &str) -> (WsClient, String, String) {
    let (mut c, login, char_id) = in_world(ws).await;
    make_admin(db_url, &login).await;
    let seq = c.send("cmd/auth.logout", json!({})).await;
    assert_eq!(c.ack(seq).await["result"], "ok", "logout");
    let seq = c
        .send(
            "cmd/auth.login",
            json!({ "login": login, "password": "longenough1" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "relogin");
    c.wait_event("evt/session.start").await;
    let seq = c
        .send("cmd/char.select", json!({ "char_id": char_id }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "select");
    c.wait_event("evt/state/snapshot").await;
    (c, login, char_id)
}

/// Полный снапшот (§3) для проверки инвентаря.
async fn snapshot(c: &mut WsClient) -> serde_json::Value {
    let seq = c
        .send("cmd/state.resync", json!({ "last_state_version": 1 }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "resync: {ack}");
    // Снапшот приходит событием (§3), не в ack.
    let snap = c.wait_event("evt/state/snapshot").await;
    snap["payload"].clone()
}

/// qty предмета `uid` в инвентаре снапшота (0 — нет).
fn qty_of(snap: &serde_json::Value, uid: &str) -> i64 {
    snap["char"]["inventory"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .find(|i| i["uid"] == uid)
                .and_then(|i| i["qty"].as_i64())
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// Игрок получает `E_PERMISSION` на все admin-команды.
#[tokio::test]
async fn admin_commands_denied_for_players() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world(&ws).await;

    let seq = c.send("cmd/admin.items", json!({})).await;
    assert_eq!(c.ack(seq).await["error"]["code"], "E_PERMISSION", "каталог");
    let seq = c
        .send(
            "cmd/admin.give.ask",
            json!({ "target": "self", "def_id": "gold_coin" }),
        )
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_PERMISSION",
        "give.ask"
    );
    let seq = c
        .send(
            "cmd/admin.remove.ask",
            json!({ "target": "self", "ref": "i_x" }),
        )
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_PERMISSION",
        "remove.ask"
    );
    let seq = c
        .send("cmd/admin.god_mode", json!({ "enabled": true }))
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_PERMISSION",
        "god_mode"
    );
}

/// Полный флоу админа: каталог → ask → диалог → выдача5 → границы
/// qty → удаление2 → удаление остатка; неизвестный def → E_ITEM_GONE.
#[tokio::test]
async fn admin_catalog_give_and_remove_flow() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world_admin(&ws, &db_url).await;

    // Каталог (вариант A — одним запросом).
    let seq = c.send("cmd/admin.items", json!({})).await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "каталог: {ack}");
    let items = ack["payload"]["items"].as_array().expect("массив items");
    assert!(items.len() > 50, "каталог полный: {}", items.len());
    let def_id = items[0]["id"].as_str().expect("id").to_string();

    // Неизвестный def → E_ITEM_GONE.
    let seq = c
        .send(
            "cmd/admin.give.ask",
            json!({ "target": "self", "def_id": "нет_такого_предмета" }),
        )
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_ITEM_GONE",
        "неизвестный def"
    );

    // Выдача: ask → execute-диалог количества.
    let seq = c
        .send(
            "cmd/admin.give.ask",
            json!({ "target": "self", "def_id": def_id }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "give.ask");
    let open = c.wait_event("evt/dialog/open").await;
    assert_eq!(open["payload"]["kind"], "execute", "kind: {open}");
    let give_id = open["payload"]["id"].as_str().expect("id").to_string();
    assert!(
        give_id.starts_with("admin.give:self#"),
        "id диалога: {give_id}"
    );
    assert_eq!(
        open["payload"]["inputs"][0]["id"], "count",
        "поле количества"
    );

    // Количество5 → предмет в рюкзаке.
    let seq = c
        .send(
            "cmd/dialog.submit",
            json!({ "dialog_id": give_id, "values": { "count": 5 } }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "submit 5");
    let added = c.wait_event("evt/item/added").await;
    let uid = added["payload"]["item"]["uid"]
        .as_str()
        .expect("uid")
        .to_string();
    let snap = snapshot(&mut c).await;
    assert_eq!(qty_of(&snap, &uid), 5, "выдано 5");

    // Границы qty: 0 и 1000 → E_BAD_QTY (диалог stateless).
    for bad in [0, 1000] {
        let seq = c
            .send(
                "cmd/dialog.submit",
                json!({ "dialog_id": give_id, "values": { "count": bad } }),
            )
            .await;
        assert_eq!(c.ack(seq).await["error"]["code"], "E_BAD_QTY", "qty {bad}");
    }

    // Удаление2 из5 → стак уменьшился.
    let seq = c
        .send(
            "cmd/admin.remove.ask",
            json!({ "target": "self", "ref": uid }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "remove.ask");
    let open = c.wait_event("evt/dialog/open").await;
    let remove_id = open["payload"]["id"].as_str().expect("id").to_string();
    assert!(
        remove_id.starts_with("admin.remove:self#"),
        "id диалога: {remove_id}"
    );
    let seq = c
        .send(
            "cmd/dialog.submit",
            json!({ "dialog_id": remove_id, "values": { "count": 2 } }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "удалить 2");
    c.wait_event("evt/item/state").await;
    let snap = snapshot(&mut c).await;
    assert_eq!(qty_of(&snap, &uid), 3, "осталось 3");

    // Удаление остатка (3 ≥ 3) → предмет исчез.
    let seq = c
        .send(
            "cmd/dialog.submit",
            json!({ "dialog_id": remove_id, "values": { "count": 3 } }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "удалить всё");
    c.wait_event("evt/item/removed").await;
    let snap = snapshot(&mut c).await;
    assert_eq!(qty_of(&snap, &uid), 0, "предмета нет");
}

/// Режим бога: только админ, toggle с эхо ack.
#[tokio::test]
async fn god_mode_toggles_for_admin() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world_admin(&ws, &db_url).await;

    let seq = c
        .send("cmd/admin.god_mode", json!({ "enabled": true }))
        .await;
    assert_eq!(c.ack(seq).await["payload"]["enabled"], true, "вкл");
    let seq = c
        .send("cmd/admin.god_mode", json!({ "enabled": false }))
        .await;
    assert_eq!(c.ack(seq).await["payload"]["enabled"], false, "выкл");
}

/// `/scenario` под гейтом (дыра из бэклога): игрок — E_PERMISSION при
/// `allow_scenario=false`, админ проходит.
#[tokio::test]
async fn scenario_gated_without_flag() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let addr = common::start_server_with(&db_url, |cfg| {
        cfg.server.allow_scenario = false;
    })
    .await;
    let ws = format!("ws://{addr}/ws");

    let (mut c, _login, _char) = in_world(&ws).await;
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": "/scenario give gold_coin" }),
        )
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_PERMISSION",
        "игрок без флага"
    );

    // Админ проходит даже с выключенным флагом.
    let (mut c, _login, _char) = in_world_admin(&ws, &db_url).await;
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": "/scenario give gold_coin" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "админ проходит");
    c.wait_event("evt/item/added").await;
}

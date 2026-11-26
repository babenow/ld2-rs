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

    // Очистка цели «Удалить все» (self): даём стак → команда → пусто.
    give_flow(&mut c, "self", "gold_coin", 5).await;
    let info = target_info(&mut c, "self").await;
    assert!(
        info.as_array()
            .expect("items")
            .iter()
            .any(|i| i["id"] == "gold_coin"),
        "до очистки: {info}"
    );
    let seq = c.send("cmd/admin.clear", json!({ "target": "self" })).await;
    assert_eq!(c.ack(seq).await["result"], "ok", "clear self");
    c.wait_event("evt/item/removed").await;
    let info = target_info(&mut c, "self").await;
    assert!(
        !info
            .as_array()
            .expect("items")
            .iter()
            .any(|i| i["id"] == "gold_coin"),
        "после очистки: {info}"
    );
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

/// M24 F24c: цели выдачи/удаления — игрок по имени (онлайн получает
/// события), лут трупа (broadcast evt/corpse/created), админ-лут
/// монстра (one-shot до респавна); неизвестная цель → E_TARGET_GONE.
#[tokio::test]
async fn admin_gives_to_corpse_monster_and_char() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut admin, _login, _char) = in_world_admin(&ws, &db_url).await;
    let (mut other, _login2, _char2) = in_world(&ws).await;

    // Имя второго игрока — из его снапшота.
    let other_snap = snapshot(&mut other).await;
    let other_name = other_snap["char"]["name"]
        .as_str()
        .expect("имя")
        .to_string();

    // ── цель: другой игрок по имени ──
    let target = format!("char:{other_name}");
    let seq = admin
        .send(
            "cmd/admin.give.ask",
            json!({ "target": target, "def_id": "gold_coin" }),
        )
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "ask char");
    let open = admin.wait_event("evt/dialog/open").await;
    let give_id = open["payload"]["id"].as_str().expect("id").to_string();
    let seq = admin
        .send(
            "cmd/dialog.submit",
            json!({ "dialog_id": give_id, "values": { "count": 3 } }),
        )
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "выдать3");
    // Онлайн-цель получает событие в свой коннект.
    let added = other.wait_event("evt/item/added").await;
    let uid = added["payload"]["item"]["uid"]
        .as_str()
        .expect("uid")
        .to_string();
    let info = {
        let seq = admin
            .send("cmd/admin.target.info", json!({ "target": target }))
            .await;
        admin.ack(seq).await
    };
    let items = info["payload"]["items"].as_array().expect("items");
    assert!(
        items.iter().any(|i| i["ref"] == uid && i["qty"] == 3),
        "инвентарь цели: {info}"
    );

    // Удаление2 из3 у цели.
    let seq = admin
        .send(
            "cmd/admin.remove.ask",
            json!({ "target": target, "ref": uid }),
        )
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "remove ask");
    let open = admin.wait_event("evt/dialog/open").await;
    let remove_id = open["payload"]["id"].as_str().expect("id").to_string();
    let seq = admin
        .send(
            "cmd/dialog.submit",
            json!({ "dialog_id": remove_id, "values": { "count": 2 } }),
        )
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "удалить2");
    other.wait_event("evt/item/state").await;

    // ── цели в лесу: труп и монстр ──
    let seq = admin
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "в лес");

    // Монстр (пока жив): админ-лут one-shot — до убийства, т.к. цель
    // должна существовать в локации.
    let monster_target = "monster:m_3".to_string();
    let seq = admin
        .send(
            "cmd/admin.give.ask",
            json!({ "target": monster_target, "def_id": "gold_coin" }),
        )
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "ask monster");
    let open = admin.wait_event("evt/dialog/open").await;
    let give_id = open["payload"]["id"].as_str().expect("id").to_string();
    let seq = admin
        .send(
            "cmd/dialog.submit",
            json!({ "dialog_id": give_id, "values": { "count": 1 } }),
        )
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "выдать монстру");
    let info = {
        let seq = admin
            .send("cmd/admin.target.info", json!({ "target": monster_target }))
            .await;
        admin.ack(seq).await
    };
    let items = info["payload"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "одна позиция: {info}");
    assert_eq!(items[0]["ref"], "bonus:0", "ref: {info}");

    let seq = admin
        .send("cmd/combat.attack", json!({ "target_id": "m_3" }))
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "атака");
    let corpse_evt = admin.wait_event("evt/corpse/created").await;
    let corpse_id = corpse_evt["payload"]["entity"]["id"]
        .as_str()
        .expect("id трупа")
        .to_string();
    admin.wait_event("evt/combat/end").await;

    // Труп: выдача2 слотов → broadcast обновления трупа; слотов
    // «до» не считаем в ассерте иначе — у волка свой дроп.
    let corpse_target = format!("corpse:{corpse_id}");
    let before_len = {
        let seq = admin
            .send("cmd/admin.target.info", json!({ "target": corpse_target }))
            .await;
        let ack = admin.ack(seq).await;
        ack["payload"]["items"].as_array().expect("items до").len()
    };
    let seq = admin
        .send(
            "cmd/admin.give.ask",
            json!({ "target": corpse_target, "def_id": "gold_coin" }),
        )
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "ask corpse");
    let open = admin.wait_event("evt/dialog/open").await;
    let give_id = open["payload"]["id"].as_str().expect("id").to_string();
    let seq = admin
        .send(
            "cmd/dialog.submit",
            json!({ "dialog_id": give_id, "values": { "count": 2 } }),
        )
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "выдать в труп");
    let update = admin.wait_event("evt/corpse/created").await;
    assert_eq!(
        update["payload"]["entity"]["id"], corpse_id,
        "broadcast трупа"
    );
    let info = {
        let seq = admin
            .send("cmd/admin.target.info", json!({ "target": corpse_target }))
            .await;
        admin.ack(seq).await
    };
    let items = info["payload"]["items"].as_array().expect("items");
    assert_eq!(
        items.len(),
        before_len + 2,
        "инъекция добавила ровно два слота: {info}"
    );
    // Инъекция добавляется в конец — последний слот наш.
    let slot_ref = items[items.len() - 1]["ref"]
        .as_str()
        .expect("ref")
        .to_string();
    assert!(slot_ref.starts_with(&format!("loot:{corpse_id}:")), "ref");

    // Удаление одного слота трупа.
    let seq = admin
        .send(
            "cmd/admin.remove.ask",
            json!({ "target": corpse_target, "ref": slot_ref }),
        )
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "remove corpse ask");
    let open = admin.wait_event("evt/dialog/open").await;
    let remove_id = open["payload"]["id"].as_str().expect("id").to_string();
    let seq = admin
        .send(
            "cmd/dialog.submit",
            json!({ "dialog_id": remove_id, "values": { "count": 1 } }),
        )
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "удалить слот");
    admin.wait_event("evt/corpse/created").await;
    let info = {
        let seq = admin
            .send("cmd/admin.target.info", json!({ "target": corpse_target }))
            .await;
        admin.ack(seq).await
    };
    let items = info["payload"]["items"].as_array().expect("items");
    assert_eq!(items.len(), before_len + 1, "слот удалён: {info}");

    // Очистка трупа целиком (кнопка «Удалить все»).
    let seq = admin
        .send("cmd/admin.clear", json!({ "target": corpse_target }))
        .await;
    assert_eq!(admin.ack(seq).await["result"], "ok", "clear corpse");
    admin.wait_event("evt/corpse/created").await;
    let info = target_info(&mut admin, &corpse_target).await;
    assert!(
        info.as_array().expect("items").is_empty(),
        "труп пуст: {info}"
    );

    // Неизвестная цель → E_TARGET_GONE.
    let seq = admin
        .send(
            "cmd/admin.give.ask",
            json!({ "target": "container:нет", "def_id": "gold_coin" }),
        )
        .await;
    assert_eq!(
        admin.ack(seq).await["error"]["code"],
        "E_TARGET_GONE",
        "неизвестная цель"
    );
}

/// M24: ask→диалог→submit выдачи (хелпер новых тестов).
async fn give_flow(c: &mut ld2_test_kit::WsClient, target: &str, def_id: &str, count: i64) {
    let seq = c
        .send(
            "cmd/admin.give.ask",
            json!({ "target": target, "def_id": def_id }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "give.ask {target}");
    let open = c.wait_event("evt/dialog/open").await;
    let id = open["payload"]["id"].as_str().expect("id").to_string();
    let seq = c
        .send(
            "cmd/dialog.submit",
            json!({ "dialog_id": id, "values": { "count": count } }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "submit {target} ×{count}");
}

/// M24: ask→диалог→submit удаления (хелпер новых тестов).
async fn remove_flow(c: &mut ld2_test_kit::WsClient, target: &str, item_ref: &str, count: i64) {
    let seq = c
        .send(
            "cmd/admin.remove.ask",
            json!({ "target": target, "ref": item_ref }),
        )
        .await;
    assert_eq!(
        c.ack(seq).await["result"],
        "ok",
        "remove.ask {target} {item_ref}"
    );
    let open = c.wait_event("evt/dialog/open").await;
    let id = open["payload"]["id"].as_str().expect("id").to_string();
    let seq = c
        .send(
            "cmd/dialog.submit",
            json!({ "dialog_id": id, "values": { "count": count } }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "submit remove ×{count}");
}

/// M24: содержимое цели (хелпер новых тестов).
async fn target_info(c: &mut ld2_test_kit::WsClient, target: &str) -> serde_json::Value {
    let seq = c
        .send("cmd/admin.target.info", json!({ "target": target }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "target.info {target}: {ack}");
    ack["payload"]["items"].clone()
}

/// M24 F24d: сундук (надстройка in-memory + tombstone базы + взятие
/// инъекции игроком) и магазин NPC (надстройка/контент-товары +
/// shop.list).
#[tokio::test]
async fn admin_gives_to_container_and_shop() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char) = in_world_admin(&ws, &db_url).await;

    // ── магазин (деревня — стартовая локация) ──
    let shop = "shop:npc_elder".to_string();
    let before = target_info(&mut c, &shop).await;
    let before_len = before.as_array().expect("items").len();
    assert!(before_len > 0, "контент магазина есть: {before}");

    // Выдача → позиция надстройки (xshop-реф) в конце списка.
    give_flow(&mut c, &shop, "gold_coin", 1).await;
    let info = target_info(&mut c, &shop).await;
    let items = info.as_array().expect("items");
    assert_eq!(items.len(), before_len + 1, "надстройка: {info}");
    let x_ref = items[items.len() - 1]["ref"]
        .as_str()
        .expect("ref")
        .to_string();
    assert!(x_ref.starts_with("xshop:"), "ref: {x_ref}");

    // Удаление надстройки.
    remove_flow(&mut c, &shop, &x_ref, 1).await;
    let info = target_info(&mut c, &shop).await;
    assert_eq!(
        info.as_array().expect("items").len(),
        before_len,
        "надстройка удалена: {info}"
    );

    // Удаление контентного товара (tombstone) + покупатели видят в
    // shop.list (следующий pull).
    let total_items = |ack: &serde_json::Value| -> usize {
        ack["payload"]["shop"]["tabs"]
            .as_array()
            .map(|tabs| {
                tabs.iter()
                    .map(|t| t["items"].as_array().map(|i| i.len()).unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0)
    };
    let seq = c
        .send("cmd/shop.list", json!({ "entity_id": "npc_elder" }))
        .await;
    let shop_ack = c.ack(seq).await;
    assert_eq!(shop_ack["result"], "ok", "shop.list: {shop_ack}");
    let shop_before = total_items(&shop_ack);
    let content_ref = before
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["ref"].as_str().is_some_and(|r| r.starts_with("shop:")))
        .expect("контентный реф")["ref"]
        .as_str()
        .expect("ref")
        .to_string();
    remove_flow(&mut c, &shop, &content_ref, 1).await;
    let info = target_info(&mut c, &shop).await;
    assert_eq!(
        info.as_array().expect("items").len(),
        before_len - 1,
        "контент удалён: {info}"
    );
    let seq = c
        .send("cmd/shop.list", json!({ "entity_id": "npc_elder" }))
        .await;
    let shop_ack = c.ack(seq).await;
    assert_eq!(total_items(&shop_ack), shop_before - 1, "shop.list");

    // Неизвестный магазин → E_TARGET_GONE.
    let seq = c
        .send(
            "cmd/admin.give.ask",
            json!({ "target": "shop:npc_нет_такого", "def_id": "gold_coin" }),
        )
        .await;
    assert_eq!(
        c.ack(seq).await["error"]["code"],
        "E_TARGET_GONE",
        "нет магазина"
    );

    // ── сундук (лес) ──
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": "e_north" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "в лес");
    let cont = "container:cont_forest_chest".to_string();
    let before = target_info(&mut c, &cont).await;
    let base_len = before.as_array().expect("items").len();
    assert_eq!(base_len, 3, "база сундука: {before}");

    // Выдача2 → ОДИН стак с указанным количеством (указание
    // заказчика): одна позиция qty=2, а не две позиции.
    give_flow(&mut c, &cont, "gold_coin", 2).await;
    let info = target_info(&mut c, &cont).await;
    let items = info.as_array().expect("items");
    assert_eq!(items.len(), base_len + 1, "один стак: {info}");
    assert_eq!(items[items.len() - 1]["qty"], 2, "количество стака: {info}");

    // Взятие стака игроком — ровно 2 монеты, позиция исчезает.
    let take_uid = format!("loot:cont_forest_chest:{base_len}");
    let seq = c
        .send("cmd/item.use", json!({ "item_uid": take_uid }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "взять стак");
    let added = c.wait_event("evt/item/added").await;
    assert_eq!(added["payload"]["item"]["qty"], 2, "взято ровно2: {added}");
    let info = target_info(&mut c, &cont).await;
    assert_eq!(
        info.as_array().expect("items").len(),
        base_len,
        "слот занят: {info}"
    );

    // Частичное удаление: стак3 → убрали1 → остаётся2 → убрали2 → пусто.
    give_flow(&mut c, &cont, "gold_coin", 3).await;
    let info = target_info(&mut c, &cont).await;
    let items = info.as_array().expect("items");
    assert_eq!(items.len(), base_len + 1, "новый стак: {info}");
    let x_ref = items[items.len() - 1]["ref"]
        .as_str()
        .expect("ref")
        .to_string();
    assert!(x_ref.starts_with("contx:"), "ref: {x_ref}");
    remove_flow(&mut c, &cont, &x_ref, 1).await;
    let info = target_info(&mut c, &cont).await;
    let items = info.as_array().expect("items");
    assert_eq!(items.len(), base_len + 1, "стак остался: {info}");
    assert_eq!(items[items.len() - 1]["qty"], 2, "остаток2: {info}");
    remove_flow(&mut c, &cont, &x_ref, 2).await;
    let info = target_info(&mut c, &cont).await;
    assert_eq!(
        info.as_array().expect("items").len(),
        base_len,
        "стак удалён целиком: {info}"
    );

    // Tombstone базового слота — исчезает из списка.
    let base_ref = info
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["ref"].as_str().is_some_and(|r| r.starts_with("cont:")))
        .expect("cont-реф")["ref"]
        .as_str()
        .expect("ref")
        .to_string();
    remove_flow(&mut c, &cont, &base_ref, 1).await;
    let info = target_info(&mut c, &cont).await;
    assert_eq!(
        info.as_array().expect("items").len(),
        base_len - 1,
        "базовый слот удалён: {info}"
    );
    // Повторное удаление того же слота → E_ITEM_GONE.
    let seq = c
        .send(
            "cmd/admin.remove.ask",
            json!({ "target": cont, "ref": base_ref }),
        )
        .await;
    let ask_ack = c.ack(seq).await;
    assert_eq!(ask_ack["result"], "ok", "повторный remove.ask");
    let open = c.wait_event("evt/dialog/open").await;
    let id = open["payload"]["id"].as_str().expect("id").to_string();
    let seq = c
        .send(
            "cmd/dialog.submit",
            json!({ "dialog_id": id, "values": { "count": 1 } }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_ITEM_GONE",
        "повторное удаление: {ack}"
    );
}

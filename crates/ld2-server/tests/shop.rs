//! M18/F39: торговый магазин NPC против реального сервера (гейт:
//! LD2_TEST_DATABASE_URL): `actions` с кнопкой магазина, цены
//! (база × наценки), покупка (валюта списана, товар получен),
//! нехватка средств → `E_NOT_ENOUGH_MONEY`, скидка аккаунта (bp).

mod common;

use common::in_world;
use serde_json::{json, Value};

/// Начислить монеты dev-сценарием (паттерн money.rs).
async fn give_gold(c: &mut ld2_test_kit::WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "gold {amount}");
}

async fn talk(c: &mut ld2_test_kit::WsClient, npc_id: &str) -> Value {
    let seq = c.send("cmd/npc.talk", json!({ "entity_id": npc_id })).await;
    c.ack(seq).await
}

async fn shop_list(c: &mut ld2_test_kit::WsClient, npc_id: &str) -> Value {
    let seq = c
        .send("cmd/shop.list", json!({ "entity_id": npc_id }))
        .await;
    c.ack(seq).await
}

async fn shop_buy(
    c: &mut ld2_test_kit::WsClient,
    npc_id: &str,
    currency: &str,
    def_id: &str,
) -> Value {
    let seq = c
        .send(
            "cmd/shop.buy",
            json!({ "entity_id": npc_id, "currency": currency, "def_id": def_id }),
        )
        .await;
    c.ack(seq).await
}

/// (def_id → цена) внутри вкладки валюты.
fn prices(ack: &Value, currency: &str) -> std::collections::HashMap<String, i64> {
    ack["payload"]["shop"]["tabs"]
        .as_array()
        .expect("вкладки")
        .iter()
        .find(|tab| tab["currency"] == currency)
        .unwrap_or_else(|| panic!("нет вкладки {currency}"))["items"]
        .as_array()
        .expect("товары")
        .iter()
        .map(|item| {
            (
                item["def_id"].as_str().expect("def_id").to_string(),
                item["price"].as_i64().expect("price"),
            )
        })
        .collect()
}

/// (сумма монет, количество партий товара) персонажа — из БД.
async fn money_and_qty(db_url: &str, char_id: &str, def_id: &str) -> (i64, i64) {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    let row: (i64, i64) = sqlx::query_as(
        "SELECT COALESCE((SELECT SUM(i.qty) FROM items i \
         JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 AND d.item_type = 'money'), 0), \
         COALESCE((SELECT SUM(qty) FROM items WHERE character_id = $1 AND def_id = $2), 0)",
    )
    .bind(char_id)
    .bind(def_id)
    .fetch_one(&pool)
    .await
    .expect("баланс");
    pool.close().await;
    row
}

/// M18/F39: кнопка магазина в диалоге, цены по формуле (база × наценка
/// торговца 1.15 при наценке сервера 1.0), покупка списывает валюту и
/// выдаёт товар, нехватка → `E_NOT_ENOUGH_MONEY` (и для второй валюты).
#[tokio::test]
async fn shop_lists_prices_and_buys() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;

    // Кнопка магазина — в actions диалога.
    let ack = talk(&mut c, "npc_elder").await;
    let actions: Vec<&str> = ack["payload"]["actions"]
        .as_array()
        .map(|values| values.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert!(actions.contains(&"shop"), "actions: {actions:?}");

    // Цены: server_markup дефолтный 1.0, наценка Старосты 1.15.
    // potion_heal 25г → floor(28.75)=28; armor_bronze 120 → 138;
    // sword_knight 180 → 207; rune_fire 40 → 46.
    let ack = shop_list(&mut c, "npc_elder").await;
    assert_eq!(ack["result"], "ok", "shop.list: {ack}");
    let gold = prices(&ack, "gold_coin");
    assert_eq!(
        gold.get("potion_heal"),
        Some(&28),
        "наценка торговца: {gold:?}"
    );
    assert_eq!(gold.get("armor_bronze"), Some(&138));
    assert_eq!(gold.get("sword_knight"), Some(&207));
    assert_eq!(gold.get("rune_fire"), Some(&46));
    // Вкладка самоцветов: rate 0.05 от золотой цены (28 → round(1.4)=1).
    let gems = prices(&ack, "gem_str");
    assert_eq!(
        gems.get("potion_heal"),
        Some(&1),
        "валюта-вкладка: {gems:?}"
    );
    assert_eq!(
        gems.get("sword_knight"),
        // Золотая цена меча 207 × rate 0.05 = 10.35 → round → 10.
        Some(&10)
    );

    // Без денег покупка не проходит.
    let ack = shop_buy(&mut c, "npc_elder", "gold_coin", "potion_heal").await;
    assert_eq!(
        ack["error"]["code"], "E_NOT_ENOUGH_MONEY",
        "без денег: {ack}"
    );

    // 30 золота → покупка зелья (28) проходит, сдача 2.
    give_gold(&mut c, 30).await;
    let ack = shop_buy(&mut c, "npc_elder", "gold_coin", "potion_heal").await;
    assert_eq!(ack["result"], "ok", "покупка: {ack}");
    let (money, potions) = money_and_qty(&db_url, &char_id, "potion_heal").await;
    assert_eq!(money, 2, "золото списано: {money}");
    assert_eq!(potions, 1, "товар получен");

    // Сдача 2 < цены меча (207) → снова нехватка.
    let ack = shop_buy(&mut c, "npc_elder", "gold_coin", "sword_knight").await;
    assert_eq!(ack["error"]["code"], "E_NOT_ENOUGH_MONEY", "дорого: {ack}");

    // Вторая валюта: самоцветов нет → нехватка.
    let ack = shop_buy(&mut c, "npc_elder", "gem_str", "potion_heal").await;
    assert_eq!(
        ack["error"]["code"], "E_NOT_ENOUGH_MONEY",
        "нет валюты-вкладки: {ack}"
    );
}

/// M18/F39: скидка аккаунта (премиум, bp) снижает цену:
/// 28 → floor(25 × 1.15 × 0.9) = 25.
#[tokio::test]
async fn shop_account_discount_lowers_price() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, login, _char_id) = in_world(&ws).await;

    let ack = shop_list(&mut c, "npc_elder").await;
    assert_eq!(prices(&ack, "gold_coin").get("potion_heal"), Some(&28));

    // Премиум: −10% (1000 bp) на аккаунт.
    let cfg = common::test_config(&db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    sqlx::query("UPDATE accounts SET shop_discount_bp = 1000 WHERE login = $1")
        .bind(&login)
        .execute(&pool)
        .await
        .expect("премиум");
    pool.close().await;

    let ack = shop_list(&mut c, "npc_elder").await;
    assert_eq!(
        prices(&ack, "gold_coin").get("potion_heal"),
        Some(&25),
        "скидка аккаунта применена"
    );
}

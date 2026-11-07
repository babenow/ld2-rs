//! Оплата у тренера предметами-квестоусловиями (M14, F5): «принеси мне
//! что-то — и я обучу» (`price.items` в маркере `trainer`).
//!
//! Отдельный test-бинарь: тест подменяет `world.content_dir` (копия
//! контента с условием у npc_elder), поэтому не должен гоняться
//! параллельно с тестами на общем контенте — версия сида в БД у всех
//! одна, а `content::seed` пересеивает её при расхождении.

mod common;

use std::path::PathBuf;

use common::in_world;
use serde_json::json;

/// Копия контента, где у npc_elder в `price.items` — трава (herb ×1).
fn content_with_item_price() -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../content");
    let dst = std::env::temp_dir().join(format!("ld2_trainer_items_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::create_dir_all(&dst).expect("tmp content");
    for entry in std::fs::read_dir(&src).expect("каталог контента") {
        let entry = entry.expect("запись");
        std::fs::copy(entry.path(), dst.join(entry.file_name())).expect("копия файла");
    }
    let loc_path = dst.join("locations.json");
    let mut root: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&loc_path).expect("читаем")).expect("json");
    let mut touched = false;
    for loc in root["locations"].as_array_mut().expect("locations") {
        for entity in loc["entities"].as_array_mut().expect("entities") {
            if entity["id"] == "npc_elder" {
                entity["trainer"]["price"]["items"] = json!([
                    { "type": "herb", "qty": 1 }
                ]);
                touched = true;
            }
        }
    }
    assert!(touched, "npc_elder найден в копии контента");
    std::fs::write(
        &loc_path,
        serde_json::to_string_pretty(&root).expect("serialize"),
    )
    .expect("пишем locations.json");
    dst
}

/// Выдать предмет dev-сценарием.
async fn give(c: &mut ld2_test_kit::WsClient, def_id: &str) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "system", "text": format!("/scenario give {def_id}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "give {def_id}");
    c.wait_event("evt/item/added").await;
}

/// Начислить золото dev-сценарием (золото списывается раньше предметов).
async fn give_gold(c: &mut ld2_test_kit::WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "gold {amount}");
}

/// Сколько травы (`def_id = herb`) осталось у персонажа.
async fn herbs_left(db_url: &str, char_id: &str) -> i64 {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    let count: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(qty), 0) FROM items WHERE character_id = $1 AND def_id = 'herb'",
    )
    .bind(char_id)
    .fetch_one(&pool)
    .await
    .expect("трава");
    pool.close().await;
    count
}

#[tokio::test]
async fn trainer_takes_quest_item_as_payment() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let content = content_with_item_price();
    let addr = common::start_server_with(&db_url, |cfg| {
        cfg.world.content_dir = content.clone();
    })
    .await;
    let ws = format!("ws://{addr}/ws");
    let (mut c, _login, char_id) = in_world(&ws).await;

    // Золото есть, предмета нет → E_INGREDIENTS с упоминанием предмета.
    give_gold(&mut c, 100).await;
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "alchemist" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_INGREDIENTS", "без предмета: {ack}");
    let msg = ack["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("herb"), "условие предмета в ответе: {ack}");

    // Принесли траву → обучение проходит, трава списана тренеру.
    give(&mut c, "herb").await;
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "alchemist" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["result"], "ok", "с предметом: {ack}");
    assert_eq!(herbs_left(&db_url, &char_id).await, 0, "трава ушла тренеру");

    // Вложение XP тоже платится предметом: без травы — отказ.
    let cfg = common::test_config(&db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    sqlx::query("UPDATE characters SET xp_pool = 1000 WHERE id = $1")
        .bind(&char_id)
        .execute(&pool)
        .await
        .expect("пул");
    pool.close().await;
    give_gold(&mut c, 100).await;
    let seq = c
        .send(
            "cmd/char.xp_invest",
            json!({ "skill_id": "sword", "points": 10 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_INGREDIENTS",
        "invest без предмета: {ack}"
    );

    // Принесли вторую траву → вложение проходит, трава списана.
    give(&mut c, "herb").await;
    let seq = c
        .send(
            "cmd/char.xp_invest",
            json!({ "skill_id": "sword", "points": 10 }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "invest с предметом");
    assert_eq!(
        herbs_left(&db_url, &char_id).await,
        0,
        "вторая трава ушла тренеру"
    );

    let _ = std::fs::remove_dir_all(&content);
}

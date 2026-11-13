//! F29a (M16): деньги — предметы-монеты. `/scenario gold` складывает
//! стак (grant_stacking), покупка у тренера списывает стак событиями
//! `evt/item/removed`/`evt/item/state`.

mod common;

use common::in_world;
use serde_json::json;

/// Начислить монеты dev-сценарием.
async fn give_gold(c: &mut ld2_test_kit::WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "gold {amount}");
}

/// (строк стаков, сумма) монет персонажа — напрямую из БД.
async fn money_rows(db_url: &str, char_id: &str) -> (i64, i64) {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    let row: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(i.qty), 0) FROM items i \
         JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 AND d.item_type = 'money'",
    )
    .bind(char_id)
    .fetch_one(&pool)
    .await
    .expect("монеты");
    pool.close().await;
    row
}

#[tokio::test]
async fn scenario_gold_stacks_and_spends_at_trainer() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
    let cfg = common::test_config(&db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    sqlx::query("UPDATE characters SET xp_pool = 1000 WHERE id = $1")
        .bind(&char_id)
        .execute(&pool)
        .await
        .expect("пул");

    // Два начисления → один стак (F29a: grant_stacking).
    give_gold(&mut c, 30).await;
    give_gold(&mut c, 70).await;
    assert_eq!(money_rows(&db_url, &char_id).await, (1, 100), "один стак");

    // M18/F44: обучение у тренера стоит 100 золота из диалога — стак
    // уходит целиком событием evt/item/removed (consumed).
    common::set_skill(&pool, &char_id, "sword", 10_000_000, 0).await;
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "обучение за монеты");
    let removed = c.wait_event("evt/item/removed").await;
    assert_eq!(removed["payload"]["reason"], "consumed", "стак списан");
    assert_eq!(money_rows(&db_url, &char_id).await, (0, 0), "стак пуст");

    // Частичное списание: 150 − 100 = 50 → патч qty в живом стаке.
    give_gold(&mut c, 150).await;
    common::set_skill(&pool, &char_id, "sword", 10_000_000, 1).await;
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "второе обучение");
    let state = c.wait_event("evt/item/state").await;
    assert_eq!(state["payload"]["patch"]["qty"], 50, "остаток стака");
    assert_eq!(money_rows(&db_url, &char_id).await, (1, 50), "частичное");
    pool.close().await;
}

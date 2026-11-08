//! Тренеры (M14, F5): обучение и вложение XP — только у тренера локации
//! (`E_NO_TRAINER`), оплата по цене маркера контента (`E_INGREDIENTS`),
//! цена растёт с рангом владения (`gold_base + gold_per_rank × ранг`).

mod common;

use common::in_world;
use serde_json::json;

/// Начислить золото dev-сценарием (M14: обучение платное).
async fn give_gold(c: &mut ld2_test_kit::WsClient, amount: i64) {
    let seq = c
        .send(
            "cmd/chat.send",
            json!({ "channel": "global", "text": format!("/scenario gold {amount}") }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "gold {amount}");
}

/// Золото персонажа напрямую из БД (проверка списаний).
async fn gold_of(db_url: &str, char_id: &str) -> i64 {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    let gold: i64 = sqlx::query_scalar("SELECT gold FROM characters WHERE id = $1")
        .bind(char_id)
        .fetch_one(&pool)
        .await
        .expect("золото");
    pool.close().await;
    gold
}

/// Наполнить XP-пул напрямую (вложение сначала проверяет пул, потом оплату).
async fn set_pool(db_url: &str, char_id: &str, points: i64) {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    sqlx::query("UPDATE characters SET xp_pool = $2 WHERE id = $1")
        .bind(char_id)
        .bind(points)
        .execute(&pool)
        .await
        .expect("пул");
    pool.close().await;
}

/// Ранг навыка напрямую в БД (для цены, зависящей от ранга).
async fn set_skill_rank(db_url: &str, char_id: &str, skill: &str, rank: i32) {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    sqlx::query(
        "INSERT INTO character_skills (character_id, skill_id, xp, rank) \
         VALUES ($1, $2, 0, $3) \
         ON CONFLICT (character_id, skill_id) DO UPDATE SET rank = EXCLUDED.rank",
    )
    .bind(char_id)
    .bind(skill)
    .bind(rank)
    .execute(&pool)
    .await
    .expect("ранг");
    pool.close().await;
}

async fn move_exit(c: &mut ld2_test_kit::WsClient, exit_id: &str) -> serde_json::Value {
    let seq = c
        .send("cmd/location.move", json!({ "exit_id": exit_id }))
        .await;
    c.ack(seq).await
}

/// Вне деревни (лес) тренера нет → `E_NO_TRAINER` и на learn, и на invest;
/// порядок: гейт тренера раньше проверки пула (0 очков ≠ «нет тренера»).
#[tokio::test]
async fn trainer_gate_blocks_learn_and_invest_outside_village() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, _char_id) = in_world(&ws).await;

    let ack = move_exit(&mut c, "e_north").await;
    assert_eq!(ack["result"], "ok", "в лес: {ack}");

    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "alchemist" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NO_TRAINER", "learn в лесу: {ack}");

    // Пул пуст — но ошибка должна быть про тренера, а не про пул.
    let seq = c
        .send(
            "cmd/char.xp_invest",
            json!({ "skill_id": "sword", "points": 10 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NO_TRAINER", "invest в лесу: {ack}");

    // В деревне у старосты тренер есть — то же обучение проходит.
    let ack = move_exit(&mut c, "e_south").await;
    assert_eq!(ack["result"], "ok", "в деревню: {ack}");
    give_gold(&mut c, 1000).await;
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "alchemist" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "learn у тренера");
}

/// Без золота — ни learn, ни invest не проходят (`E_INGREDIENTS`);
/// ровно на цену — проходят и списывают её целиком.
#[tokio::test]
async fn learn_and_invest_are_paid_and_fail_without_gold() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;
    set_pool(&db_url, &char_id, 1000).await;

    // Нет золота → E_INGREDIENTS (цена ранга 0 = gold_base = 100).
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "alchemist" }))
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_INGREDIENTS",
        "learn без золота: {ack}"
    );

    let seq = c
        .send(
            "cmd/char.xp_invest",
            json!({ "skill_id": "sword", "points": 10 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_INGREDIENTS",
        "invest без золота: {ack}"
    );

    // Ровно базовая цена → learn проходит, золото уходит полностью.
    give_gold(&mut c, 100).await;
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "alchemist" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "learn за 100");
    assert_eq!(gold_of(&db_url, &char_id).await, 0, "золото списано");

    // Повторное изучение — идемпотентный ack без повторной оплаты:
    // золота нет (0), но learn проходит (иначе E_INGREDIENTS).
    let seq = c
        .send("cmd/craft.learn", json!({ "profession_id": "alchemist" }))
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "повтор learn без золота");
    assert_eq!(
        gold_of(&db_url, &char_id).await,
        0,
        "повтор learn не списал золото"
    );

    // Invest снова требует оплату (золота нет) → E_INGREDIENTS.
    let seq = c
        .send(
            "cmd/char.xp_invest",
            json!({ "skill_id": "sword", "points": 10 }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_INGREDIENTS",
        "invest без золота после learn: {ack}"
    );
}

/// Цена вложения растёт с рангом навыка: `100 + 50 × 3 = 250`.
#[tokio::test]
async fn trainer_price_grows_with_rank() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;

    set_skill_rank(&db_url, &char_id, "sword", 3).await;
    set_pool(&db_url, &char_id, 1000).await;
    give_gold(&mut c, 1000).await;

    let seq = c
        .send(
            "cmd/char.xp_invest",
            json!({ "skill_id": "sword", "points": 10 }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "инвест у тренера");
    assert_eq!(
        gold_of(&db_url, &char_id).await,
        1000 - (100 + 50 * 3),
        "цена ранга 3: 100 + 50×3"
    );
}

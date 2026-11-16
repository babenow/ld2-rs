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

/// Золото персонажа напрямую из БД (F29a: стаки монет type=money).
async fn gold_of(db_url: &str, char_id: &str) -> i64 {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    let gold: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(i.qty), 0) FROM items i \
         JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 AND d.item_type = 'money'",
    )
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

/// M18/F44: шкала навыка «полная» (100 %) — условие повышения ранга
/// (ADR-009); xp с большим запасом, чтобы не дублировать формулу.
async fn fill_bar(db_url: &str, char_id: &str, skill: &str, rank: i32) {
    let cfg = common::test_config(db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    common::set_skill(&pool, char_id, skill, 10_000_000, rank).await;
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

    // Пул/шкала не заполнены — но ошибка должна быть про тренера.
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(ack["error"]["code"], "E_NO_TRAINER", "train в лесу: {ack}");

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

    fill_bar(&db_url, &char_id, "sword", 0).await;
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_INGREDIENTS",
        "train без золота: {ack}"
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

    // Повторное обучение снова требует оплату (золота нет) → E_INGREDIENTS.
    fill_bar(&db_url, &char_id, "sword", 1).await;
    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    let ack = c.ack(seq).await;
    assert_eq!(
        ack["error"]["code"], "E_INGREDIENTS",
        "train без золота после learn: {ack}"
    );
}

/// M18/F44: цена обучения — из диалога и **не растёт с рангом**
/// (ADR-009): 100 золота на любом ранге (прежний рост
/// `gold_base + gold_per_rank × ранг` остался у профессий).
#[tokio::test]
async fn trainer_fee_is_flat_from_dialog() {
    let Some(db_url) = common::test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let ws = common::start_ws_server(&db_url).await;
    let (mut c, _login, char_id) = in_world(&ws).await;

    fill_bar(&db_url, &char_id, "sword", 3).await;
    set_pool(&db_url, &char_id, 1000).await;
    give_gold(&mut c, 1000).await;

    let seq = c
        .send(
            "cmd/skill.train",
            json!({ "entity_id": "npc_elder", "skill_id": "sword" }),
        )
        .await;
    assert_eq!(c.ack(seq).await["result"], "ok", "обучение у тренера");
    assert_eq!(
        gold_of(&db_url, &char_id).await,
        900,
        "фиксированная цена из диалога: 100"
    );
    let cfg = common::test_config(&db_url, |_| {});
    let pool = ld2_server::db::pool(&cfg.database).await.expect("pool");
    let left: i64 = sqlx::query_scalar("SELECT xp_pool FROM characters WHERE id = $1")
        .bind(&char_id)
        .fetch_one(&pool)
        .await
        .expect("пул");
    pool.close().await;
    assert_eq!(left, 990, "списано 10 очков опыта");
}

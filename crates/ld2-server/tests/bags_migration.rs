//! M28/B10: миграция существующих персонажей на сумки (гейт:
//! `LD2_TEST_DATABASE_URL`). Отдельный тестовый бинарь: `ensure_bags`
//! проходит по всем персонажам без сумок, и в общем бинаре с `bags.rs`
//! мог бы выдать сумки персонажу параллельного теста.

use std::collections::HashSet;

use ld2_server::config::DatabaseSection;
use ld2_server::db;

fn test_url() -> Option<String> {
    std::env::var("LD2_TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

/// Легаси-персонаж: аккаунт, герой и определения сумки/предмета.
async fn setup(pool: &sqlx::PgPool, uniq: &str) -> (String, String, String) {
    let account_id: i64 = sqlx::query_scalar(
        "INSERT INTO accounts (login, password_hash) VALUES ($1, 'x') RETURNING id",
    )
    .bind(format!("migration_{uniq}"))
    .fetch_one(pool)
    .await
    .expect("account");
    let char_id: String = sqlx::query_scalar(
        "INSERT INTO characters (account_id, name, profile) \
         VALUES ($1, $2, 'warrior') RETURNING id",
    )
    .bind(account_id)
    .bind(format!("Миграция {uniq}"))
    .fetch_one(pool)
    .await
    .expect("character");
    let bag_def = format!("t_mig_bag_{uniq}");
    let item_def = format!("t_mig_item_{uniq}");
    for (id, ty, slots) in [(&bag_def, "bag", Some(20)), (&item_def, "material", None)] {
        sqlx::query(
            "INSERT INTO item_defs (id, item_type, name, rarity, gold_value, bag_slots) \
             VALUES ($1, $2, 'Тест', 'common', 1, $3)",
        )
        .bind(id)
        .bind(ty)
        .bind(slots)
        .execute(pool)
        .await
        .expect("item_def");
    }
    (char_id, bag_def, item_def)
}

/// Предметы персонажа без самих сумок: `(uid, qty)`.
async fn loot(pool: &sqlx::PgPool, char_id: &str, bag_def: &str) -> Vec<(String, i32)> {
    sqlx::query_as(
        "SELECT uid, qty FROM items \
         WHERE character_id = $1 AND def_id <> $2 ORDER BY uid",
    )
    .bind(char_id)
    .bind(bag_def)
    .fetch_all(pool)
    .await
    .expect("items")
}

async fn bag_count(pool: &sqlx::PgPool, char_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM items WHERE character_id = $1 AND container = 'bags'")
        .bind(char_id)
        .fetch_one(pool)
        .await
        .expect("bags count")
}

#[tokio::test]
async fn ensure_bags_migrates_existing_character_without_losing_items() {
    let Some(url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let pool = db::connect(&DatabaseSection {
        url,
        max_connections: 4,
    })
    .await
    .expect("connect");
    let uniq = format!("m_{}", std::process::id());
    let (char_id, bag_def, item_def) = setup(&pool, &uniq).await;

    // Старый персонаж: 30 предметов в рюкзаке, слотов и сумок ещё нет.
    let mut inserted = Vec::new();
    for _ in 0..30 {
        let item = db::items::insert(&pool, &char_id, &item_def, 1)
            .await
            .expect("insert item");
        inserted.push(item.uid);
    }
    let before = loot(&pool, &char_id, &bag_def).await;
    assert_eq!(before.len(), 30, "подготовка: все предметы в рюкзаке");
    assert_eq!(bag_count(&pool, &char_id).await, 0);

    let granted = db::items::ensure_bags(&pool, 4, &bag_def)
        .await
        .expect("ensure_bags");
    assert!(
        granted >= 1,
        "миграция должна выдать сумки хотя бы этому персонажу: {granted}"
    );

    // Сумки выданы, флаг выставлен, ни один предмет не потерян.
    assert_eq!(bag_count(&pool, &char_id).await, 4, "комплект из 4 сумок");
    let flag: bool = sqlx::query_scalar("SELECT bags_granted FROM characters WHERE id = $1")
        .bind(&char_id)
        .fetch_one(&pool)
        .await
        .expect("bags_granted");
    assert!(flag, "флаг миграции выставлен");

    let after = loot(&pool, &char_id, &bag_def).await;
    let after_uids: HashSet<&str> = after.iter().map(|(uid, _)| uid.as_str()).collect();
    for uid in &inserted {
        assert!(
            after_uids.contains(uid.as_str()),
            "потерян предмет {uid} при миграции"
        );
    }
    assert_eq!(
        after.iter().map(|(_, qty)| qty).sum::<i32>(),
        before.iter().map(|(_, qty)| qty).sum::<i32>(),
        "суммарное количество предметов не изменилось"
    );

    // Повторный запуск идемпотентен: сумок по-прежнему 4, предметы на месте.
    let again = db::items::ensure_bags(&pool, 4, &bag_def)
        .await
        .expect("ensure_bags again");
    assert_eq!(again, 0, "повторная миграция не находит работу");
    assert_eq!(bag_count(&pool, &char_id).await, 4);
    assert_eq!(loot(&pool, &char_id, &bag_def).await.len(), 30);
}

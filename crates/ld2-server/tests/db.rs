//! Миграции против настоящего PostgreSQL (гейт: LD2_TEST_DATABASE_URL):
//! идемпотентность up, **полный откат вниз** (up/down-пары, `undo(0)`) и
//! повторный up — цикл «понизить версию БД и поднять снова».

use ld2_server::config::DatabaseSection;
use ld2_server::db;

fn test_url() -> Option<String> {
    std::env::var("LD2_TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

/// Все таблицы `public` (отсортированы — сравнение как множество).
async fn schema_tables(pool: &sqlx::PgPool) -> Vec<String> {
    let mut tables: Vec<String> =
        sqlx::query_scalar("SELECT tablename FROM pg_tables WHERE schemaname = 'public'")
            .fetch_all(pool)
            .await
            .expect("pg_tables");
    tables.sort();
    tables
}

#[tokio::test]
async fn migrations_up_idempotent_and_revert_cycle() {
    let Some(url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let cfg = DatabaseSection {
        url,
        max_connections: 4,
    };

    // Up дважды — идемпотентно (лог `_sqlx_migrations`; `meta` может нести
    // строки сида контента с M12.3 — «пустота meta» здесь не проверяется).
    let first = db::connect(&cfg).await.expect("первый up");
    let second = db::connect(&cfg).await.expect("повторный up");
    db::ping(&second).await.expect("ping");

    // Полный откат до версии 0: down-пары убирают всю схему, остаётся
    // только пустой лог миграций.
    let migrator = sqlx::migrate!("./migrations");
    migrator.undo(&second, 0).await.expect("undo(0)");
    let remaining = schema_tables(&second).await;
    assert_eq!(
        remaining,
        vec!["_sqlx_migrations".to_string()],
        "после undo(0) — только пустой лог миграций"
    );

    // И снова up — схема полностью восстанавливается.
    migrator.run(&second).await.expect("up после отката");
    let mut expected = [
        "_sqlx_migrations",
        "accounts",
        "character_skills",
        "characters",
        "item_defs",
        "items",
        "locations",
        "loot_taken",
        "meta",
        "refresh_tokens",
        "skills",
    ]
    .map(|s| s.to_string());
    expected.sort();
    assert_eq!(
        schema_tables(&second).await,
        expected,
        "после повторного up схема полностью восстановлена"
    );
    db::ping(&second).await.expect("ping после цикла");

    first.close().await;
    second.close().await;
}

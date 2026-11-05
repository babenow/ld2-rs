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

/// Все таблицы текущей схемы (для проверки «ушла/вернулась»).
const SCHEMA_TABLES: &str = "SELECT COUNT(*) FROM information_schema.tables \
     WHERE table_schema = 'public' \
     AND table_name IN ('meta', 'accounts', 'refresh_tokens', 'characters')";

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

    // Up дважды — идемпотентно (лог `_sqlx_migrations`).
    let first = db::connect(&cfg).await.expect("первый up");
    let second = db::connect(&cfg).await.expect("повторный up");
    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM meta")
        .fetch_one(&second)
        .await
        .expect("таблица meta существует и пуста");
    assert_eq!(rows, 0);
    db::ping(&second).await.expect("ping");

    // Полный откат до версии 0: down-пары убирают всю схему.
    let migrator = sqlx::migrate!("./migrations");
    migrator.undo(&second, 0).await.expect("undo(0)");
    let gone: i64 = sqlx::query_scalar(SCHEMA_TABLES)
        .fetch_one(&second)
        .await
        .expect("information_schema после отката");
    assert_eq!(gone, 0, "после отката до 0 схемы нет");

    // И снова up — схема полностью восстанавливается.
    migrator.run(&second).await.expect("up после отката");
    let back: i64 = sqlx::query_scalar(SCHEMA_TABLES)
        .fetch_one(&second)
        .await
        .expect("information_schema после подъёма");
    assert_eq!(back, 4, "после повторного up все таблицы на месте");
    db::ping(&second).await.expect("ping после цикла");

    first.close().await;
    second.close().await;
}

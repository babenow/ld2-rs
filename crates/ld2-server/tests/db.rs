//! Миграции против настоящего PostgreSQL (D24).
//!
//! Гейт: `LD2_TEST_DATABASE_URL` — без переменной тест печатает «пропуск» и
//! выходит (матрица CI 3 ОС без сервиса БД остаётся зелёной); в CI-job
//! `server` переменная задаётся сервисом `postgres:16`.

use ld2_server::config::DatabaseSection;
use ld2_server::db;

fn test_url() -> Option<String> {
    std::env::var("LD2_TEST_DATABASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
}

#[tokio::test]
async fn migrations_apply_idempotently() {
    let Some(url) = test_url() else {
        eprintln!("пропуск: LD2_TEST_DATABASE_URL не задан");
        return;
    };
    let cfg = DatabaseSection {
        url,
        max_connections: 4,
    };

    let first = db::connect(&cfg).await.expect("первый прогон миграций");
    let second = db::connect(&cfg)
        .await
        .expect("повторный прогон миграций (идемпотентность)");

    let (rows,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM meta")
        .fetch_one(&second)
        .await
        .expect("таблица meta существует и пуста");
    assert_eq!(rows, 0);

    db::ping(&second).await.expect("ping");

    first.close().await;
    second.close().await;
}

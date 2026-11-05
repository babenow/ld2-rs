//! PostgreSQL: пул соединений и миграции (D24, PLAN-M12).
//!
//! Миграции — `migrations/*.sql` (sqlx): применяются при каждом подключении,
//! отслеживаются в `_sqlx_migrations` — повторный запуск идемпотентен.
//! Сборка не требует DATABASE_URL (runtime-запросы, без `query!`).

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::config::DatabaseSection;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("не удалось подключиться к PostgreSQL (database.url): {0}")]
    Connect(#[source] sqlx::Error),
    #[error("ошибка при применении миграций: {0}")]
    Migrate(#[source] sqlx::migrate::MigrateError),
}

/// Пул + миграции. Любая ошибка — fail-fast до старта слушателя.
pub async fn connect(cfg: &DatabaseSection) -> Result<PgPool, DbError> {
    let pool = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .connect(&cfg.url)
        .await
        .map_err(DbError::Connect)?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .map_err(DbError::Migrate)?;
    Ok(pool)
}

/// Проверка живости пула для `/healthz`.
pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1").execute(pool).await.map(|_| ())
}

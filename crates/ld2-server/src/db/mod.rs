//! PostgreSQL: пул соединений и миграции (D24, PLAN-M12).
//!
//! Миграции — `migrations/*.sql` в **реверсируемой паре**: `<N>_<имя>.up.sql`
//! применяется при старте, `<N>_<имя>.down.sql` — откат (`ld2-server revert`,
//! одна версия вниз; данные откаченной миграции теряются). Лог применённых —
//! `_sqlx_migrations`; повторный запуск up идемпотентен. Сборка не требует
//! DATABASE_URL (runtime-запросы, без `query!`).
//!
//! Репозитории по доменам: [`accounts`] — аккаунты и токены (M12.2);
//! репозиторий персонажей появится в фазе «персонажи».

pub mod accounts;
pub mod characters;
pub mod items;
pub mod locations;
pub mod skills;

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

/// Пул **без** миграций — для `revert` (откат не должен сначала поднимать
/// схему) и для ручного управления.
pub async fn pool(cfg: &DatabaseSection) -> Result<PgPool, DbError> {
    PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .connect(&cfg.url)
        .await
        .map_err(DbError::Connect)
}

/// Применить все неприменённые миграции (up; пары down при этом не трогаются).
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(DbError::Migrate)
}

/// Пул + миграции — основной путь (`run`/`check`/`migrate`). Fail-fast до
/// старта слушателя.
pub async fn connect(cfg: &DatabaseSection) -> Result<PgPool, DbError> {
    let pool = pool(cfg).await?;
    migrate(&pool).await?;
    Ok(pool)
}

/// Проверка живости пула для `/healthz`.
pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1").execute(pool).await.map(|_| ())
}

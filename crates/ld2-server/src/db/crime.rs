//! Преступность (ФР-11, §10, миграция 0006): статусы с абсолютным
//! `expires_at` и журнал нарушений — **переживают рестарт** (D27).

use sqlx::PgPool;

/// Активный статус `(kind, expires_at)` — epoch-секунды.
pub type StatusRow = (String, i64);

/// Активные статусы персонажа (не истёкшие на `now`), по kind.
pub async fn active_statuses(
    pool: &PgPool,
    char_id: &str,
    now: i64,
) -> Result<Vec<StatusRow>, sqlx::Error> {
    sqlx::query_as::<_, StatusRow>(
        "SELECT kind, EXTRACT(EPOCH FROM expires_at)::BIGINT \
         FROM crime_statuses \
         WHERE character_id = $1 AND expires_at > to_timestamp($2) \
         ORDER BY kind",
    )
    .bind(char_id)
    .bind(now as f64)
    .fetch_all(pool)
    .await
}

/// Снять истёкшие статусы → списки `kind` для `evt/crime/removed`.
pub async fn expire(pool: &PgPool, char_id: &str, now: i64) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "DELETE FROM crime_statuses \
         WHERE character_id = $1 AND expires_at <= to_timestamp($2) \
         RETURNING kind",
    )
    .bind(char_id)
    .bind(now as f64)
    .fetch_all(pool)
    .await
}

/// Добавить/продлить статус (upsert: берётся более поздний срок) + строка
/// журнала — одной транзакцией (D27) → `(expires_at, entry_id)`.
pub async fn add_status(
    pool: &PgPool,
    char_id: &str,
    kind: &str,
    now: i64,
    delta_s: i64,
    total_s: i64,
) -> Result<(i64, i64), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let expires_at = now + delta_s;
    sqlx::query(
        "INSERT INTO crime_statuses (character_id, kind, expires_at) \
         VALUES ($1, $2, to_timestamp($3)) \
         ON CONFLICT (character_id, kind) DO UPDATE SET \
           expires_at = GREATEST(crime_statuses.expires_at, EXCLUDED.expires_at)",
    )
    .bind(char_id)
    .bind(kind)
    .bind(expires_at as f64)
    .execute(&mut *tx)
    .await?;
    let entry_id: i64 = sqlx::query_scalar(
        "INSERT INTO crime_log (character_id, kind, at, delta_s, total_s) \
         VALUES ($1, $2, to_timestamp($3), $4, $5) RETURNING id",
    )
    .bind(char_id)
    .bind(kind)
    .bind(now as f64)
    .bind(delta_s)
    .bind(total_s)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((expires_at, entry_id))
}

/// Журнал нарушений (новые первые), не больше `limit` строк.
pub async fn log(
    pool: &PgPool,
    char_id: &str,
    limit: i64,
) -> Result<Vec<(String, i64, i64, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (String, i64, i64, i64)>(
        "SELECT kind, EXTRACT(EPOCH FROM at)::BIGINT, delta_s, total_s \
         FROM crime_log WHERE character_id = $1 \
         ORDER BY id DESC LIMIT $2",
    )
    .bind(char_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Полный срок (сумма активных остатков, §10 «текущий общий срок»).
pub async fn total_term(pool: &PgPool, char_id: &str, now: i64) -> Result<i64, sqlx::Error> {
    let (total,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(\
           SUM(GREATEST(EXTRACT(EPOCH FROM expires_at)::BIGINT - $2, 0)), 0)::BIGINT \
         FROM crime_statuses WHERE character_id = $1",
    )
    .bind(char_id)
    .bind(now)
    .fetch_one(pool)
    .await?;
    Ok(total)
}

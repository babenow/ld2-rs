//! M18/F58d: баффы рун (ADR-012 §6) — стак с лимитом `max_stacks` и
//! таймером снятия. Паттерн `crime_statuses`: ключ
//! `(character_id, kind)`, **переживает рестарт** (D27).

use sqlx::PgPool;

/// Активный бафф `(kind, title, stacks, expires_at, param, value, pct)`.
pub type BuffRow = (String, String, i32, i64, String, i64, bool);

/// Активные (не истёкшие на `now`) баффы персонажа — для
/// `CharacterState.buffs` и применения к параметрам.
pub async fn active(pool: &PgPool, char_id: &str, now: i64) -> Result<Vec<BuffRow>, sqlx::Error> {
    sqlx::query_as::<_, BuffRow>(
        "SELECT kind, title, stacks, \
                EXTRACT(EPOCH FROM expires_at)::BIGINT, param, value, pct \
         FROM char_buffs \
         WHERE character_id = $1 AND expires_at > to_timestamp($2) \
         ORDER BY kind",
    )
    .bind(char_id)
    .bind(now)
    .fetch_all(pool)
    .await
}

/// Наложить бафф: стак `+1` до `max_stacks` (сверх лимита не
/// накладывается — ADR-012 §6), срок — `now + duration_s` (каждое
/// применение продлевает).
#[allow(clippy::too_many_arguments)]
pub async fn apply(
    pool: &PgPool,
    char_id: &str,
    kind: &str,
    title: &str,
    param: &str,
    value: i64,
    pct: bool,
    duration_s: i64,
    max_stacks: i32,
    now: i64,
) -> Result<(), sqlx::Error> {
    let max_stacks = max_stacks.max(1);
    sqlx::query(
        "INSERT INTO char_buffs \
           (character_id, kind, title, param, value, pct, stacks, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 1, to_timestamp($7)) \
         ON CONFLICT (character_id, kind) DO UPDATE SET \
           title = EXCLUDED.title, value = EXCLUDED.value, \
           pct = EXCLUDED.pct, \
           stacks = LEAST(char_buffs.stacks + 1, $8), \
           expires_at = EXCLUDED.expires_at",
    )
    .bind(char_id)
    .bind(kind)
    .bind(title)
    .bind(param)
    .bind(value)
    .bind(pct)
    .bind(now + duration_s)
    .bind(max_stacks)
    .execute(pool)
    .await?;
    Ok(())
}

/// Снять истёкшие баффы → `kind` для рассылки (полная замена списка —
/// `evt/char/buffs`).
pub async fn expire(pool: &PgPool, char_id: &str, now: i64) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "DELETE FROM char_buffs \
         WHERE character_id = $1 AND expires_at <= to_timestamp($2) \
         RETURNING kind",
    )
    .bind(char_id)
    .bind(now)
    .fetch_all(pool)
    .await
}

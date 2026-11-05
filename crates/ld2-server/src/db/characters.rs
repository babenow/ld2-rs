//! Репозиторий персонажей (миграция 0003, ФР-02): живые строки аккаунта,
//! soft delete, монотонный `state_version` (§3).

use sqlx::PgPool;

/// Строка `characters` для сборки §5/§3-состояния.
#[derive(Debug, sqlx::FromRow)]
pub struct CharacterRow {
    pub id: String,
    pub name: String,
    pub profile: String,
    pub location_id: String,
    pub hp_cur: i32,
    pub hp_max: i32,
    pub mp_cur: i32,
    pub mp_max: i32,
    pub xp_pool: i64,
    pub state_version: i64,
}

/// Живые персонажи аккаунта (для `char.list` и `evt/session.start` §5).
pub async fn list(pool: &PgPool, account_id: i64) -> Result<Vec<CharacterRow>, sqlx::Error> {
    sqlx::query_as::<_, CharacterRow>(
        "SELECT id, name, profile, location_id, hp_cur, hp_max, \
         mp_cur, mp_max, xp_pool, state_version \
         FROM characters \
         WHERE account_id = $1 AND deleted_at IS NULL ORDER BY created_at",
    )
    .bind(account_id)
    .fetch_all(pool)
    .await
}

/// Сколько живых персонажей у аккаунта (лимит слотов, D25/DoD M2).
pub async fn count_alive(pool: &PgPool, account_id: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM characters \
         WHERE account_id = $1 AND deleted_at IS NULL",
    )
    .bind(account_id)
    .fetch_one(pool)
    .await
}

/// Новый персонаж; `None` — имя занято среди живых (частичный UNIQUE-индекс
/// → `E_NAME_TAKEN`).
pub async fn create(
    pool: &PgPool,
    account_id: i64,
    name: &str,
    profile: &str,
) -> Result<Option<CharacterRow>, sqlx::Error> {
    sqlx::query_as::<_, CharacterRow>(
        "INSERT INTO characters (account_id, name, profile) VALUES ($1, $2, $3) \
         ON CONFLICT (name) WHERE deleted_at IS NULL DO NOTHING \
         RETURNING id, name, profile, location_id, hp_cur, hp_max, \
                  mp_cur, mp_max, xp_pool, state_version",
    )
    .bind(account_id)
    .bind(name)
    .bind(profile)
    .fetch_optional(pool)
    .await
}

/// Персонаж аккаунта (живой); `None` — не найден/чужой → «персонаж не найден».
pub async fn find(
    pool: &PgPool,
    account_id: i64,
    char_id: &str,
) -> Result<Option<CharacterRow>, sqlx::Error> {
    sqlx::query_as::<_, CharacterRow>(
        "SELECT id, name, profile, location_id, hp_cur, hp_max, \
         mp_cur, mp_max, xp_pool, state_version \
         FROM characters \
         WHERE id = $1 AND account_id = $2 AND deleted_at IS NULL",
    )
    .bind(char_id)
    .bind(account_id)
    .fetch_optional(pool)
    .await
}

/// Атомарный бамп `state_version` (§3) → новая версия.
pub async fn bump_state_version(pool: &PgPool, char_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE characters SET state_version = state_version + 1 \
         WHERE id = $1 RETURNING state_version",
    )
    .bind(char_id)
    .fetch_one(pool)
    .await
}

/// Soft delete (имя освобождается через частичный индекс).
pub async fn soft_delete(
    pool: &PgPool,
    account_id: i64,
    char_id: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE characters SET deleted_at = now() \
         WHERE id = $1 AND account_id = $2 AND deleted_at IS NULL",
    )
    .bind(char_id)
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

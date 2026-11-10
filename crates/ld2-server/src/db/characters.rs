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
    /// M15/F24: весь опыт за убийства — уровень персонажа.
    pub total_xp: i64,
    pub state_version: i64,
    /// Базовые/квестовые атрибуты (skills.md §1; JSONB, старт — все 10).
    pub attrs: serde_json::Value,
}

/// Живые персонажи аккаунта (для `char.list` и `evt/session.start` §5).
pub async fn list(pool: &PgPool, account_id: i64) -> Result<Vec<CharacterRow>, sqlx::Error> {
    sqlx::query_as::<_, CharacterRow>(
        "SELECT id, name, profile, location_id, hp_cur, hp_max, \
         mp_cur, mp_max, xp_pool, total_xp, state_version, attrs \
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
                  mp_cur, mp_max, xp_pool, total_xp, state_version, attrs",
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
         mp_cur, mp_max, xp_pool, total_xp, state_version, attrs \
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

/// Строка персонажа без проверки владельца (внутренние нужды сессии:
/// перезагрузка состояния после мутаций).
pub async fn get(pool: &PgPool, char_id: &str) -> Result<Option<CharacterRow>, sqlx::Error> {
    sqlx::query_as::<_, CharacterRow>(
        "SELECT id, name, profile, location_id, hp_cur, hp_max, \
         mp_cur, mp_max, xp_pool, total_xp, state_version, attrs \
         FROM characters WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(char_id)
    .fetch_optional(pool)
    .await
}

/// Текущее HP персонажа (зелья, ФР-07).
pub async fn set_hp(pool: &PgPool, char_id: &str, hp_cur: i32) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE characters SET hp_cur = $2 WHERE id = $1")
        .bind(char_id)
        .bind(hp_cur)
        .execute(pool)
        .await?;
    Ok(())
}

/// Текущая мана (реген по тику, §2.4; M12.5b).
pub async fn set_mp(pool: &PgPool, char_id: &str, mp_cur: i32) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE characters SET mp_cur = $2 WHERE id = $1")
        .bind(char_id)
        .bind(mp_cur)
        .execute(pool)
        .await?;
    Ok(())
}

/// Респаун (смерть, ФР-06): hp полное + позиция = спавн + бамп версии
/// одним UPDATE (D27) → `(hp_cur, hp_max, state_version)`.
pub async fn respawn(
    pool: &PgPool,
    char_id: &str,
    location_id: &str,
) -> Result<Option<(i32, i32, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (i32, i32, i64)>(
        "UPDATE characters SET hp_cur = hp_max, location_id = $2, \
         state_version = state_version + 1 WHERE id = $1 \
         RETURNING hp_cur, hp_max, state_version",
    )
    .bind(char_id)
    .bind(location_id)
    .fetch_optional(pool)
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

/// Начислить XP в пул (награды квестов, D3/D4) → новый пул.
pub async fn add_xp_pool(pool: &PgPool, char_id: &str, amount: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE characters SET xp_pool = xp_pool + $2 WHERE id = $1 RETURNING xp_pool",
    )
    .bind(char_id)
    .bind(amount)
    .fetch_one(pool)
    .await
}

/// M15/F24: накопить общий опыт за убийства — растёт всегда, даже
/// когда навык на гейте/потолке и его XP не изменился.
pub async fn add_total_xp(pool: &PgPool, char_id: &str, amount: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE characters SET total_xp = total_xp + $2 \
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(char_id)
    .bind(amount)
    .execute(pool)
    .await?;
    Ok(())
}

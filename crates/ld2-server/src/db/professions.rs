//! Профессии персонажа (skills.md §5, M12.6): изучение, XP/ранги.

use sqlx::PgPool;

/// Изученная профессия `(profession_id, xp, rank)`.
pub type LearnedRow = (String, i64, i32);

/// Профессия из сида `(id, name, prof_type)`.
pub type ProfRow = (String, String, String);

/// Все профессии контента (для `craft.list`/валидации `craft.learn`).
pub async fn content(pool: &PgPool) -> Result<Vec<ProfRow>, sqlx::Error> {
    sqlx::query_as::<_, ProfRow>("SELECT id, name, prof_type FROM professions ORDER BY id")
        .fetch_all(pool)
        .await
}

/// Изученные профессии персонажа (для снапшота и `craft.list`).
pub async fn learned(pool: &PgPool, char_id: &str) -> Result<Vec<LearnedRow>, sqlx::Error> {
    sqlx::query_as::<_, LearnedRow>(
        "SELECT profession_id, xp, rank FROM character_professions \
         WHERE character_id = $1 ORDER BY profession_id",
    )
    .bind(char_id)
    .fetch_all(pool)
    .await
}

/// Сколько **первичных** профессий изучено (лимит `profession_limit`).
pub async fn primary_count(pool: &PgPool, char_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM character_professions cp \
         JOIN professions p ON p.id = cp.profession_id \
         WHERE cp.character_id = $1 AND p.prof_type = 'primary'",
    )
    .bind(char_id)
    .fetch_one(pool)
    .await
}

/// Изучить профессию (rank 1, xp 0); `false` — уже изучена.
pub async fn learn(pool: &PgPool, char_id: &str, profession_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO character_professions (character_id, profession_id) \
         VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(char_id)
    .bind(profession_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Вклад XP в профессию (апсерт) → `(xp, rank)` после применения.
pub async fn grant_xp(
    pool: &PgPool,
    char_id: &str,
    profession_id: &str,
    xp: i64,
    rank: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO character_professions (character_id, profession_id, xp, rank) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (character_id, profession_id) DO UPDATE SET \
           xp = EXCLUDED.xp, rank = EXCLUDED.rank",
    )
    .bind(char_id)
    .bind(profession_id)
    .bind(xp)
    .bind(rank)
    .execute(pool)
    .await?;
    Ok(())
}

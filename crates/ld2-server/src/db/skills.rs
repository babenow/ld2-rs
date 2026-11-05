//! Навыки: контент и вложенные строки персонажа (skills.md §2, M12.4).

use sqlx::PgPool;

/// Навык из сида (`content/skills.json` → таблица `skills`).
#[derive(Debug, sqlx::FromRow)]
pub struct SkillContentRow {
    pub id: String,
    pub name: String,
}

/// Вложенный навык персонажа.
#[derive(Debug, sqlx::FromRow)]
pub struct SkillRow {
    pub skill_id: String,
    pub xp: i64,
    pub rank: i32,
}

/// Все навыки контента (снапшот §6 отдаёт их всех — у невложенных нули).
pub async fn content_skills(pool: &PgPool) -> Result<Vec<SkillContentRow>, sqlx::Error> {
    sqlx::query_as::<_, SkillContentRow>("SELECT id, name FROM skills ORDER BY id")
        .fetch_all(pool)
        .await
}

/// Один навык контента (валидация `char.xp_invest`); `None` — нет в сиде.
pub async fn content_skill(
    pool: &PgPool,
    skill_id: &str,
) -> Result<Option<SkillContentRow>, sqlx::Error> {
    sqlx::query_as::<_, SkillContentRow>("SELECT id, name FROM skills WHERE id = $1")
        .bind(skill_id)
        .fetch_optional(pool)
        .await
}

/// Вложенные навыки персонажа.
pub async fn learned(pool: &PgPool, char_id: &str) -> Result<Vec<SkillRow>, sqlx::Error> {
    sqlx::query_as::<_, SkillRow>(
        "SELECT skill_id, xp, rank FROM character_skills WHERE character_id = $1",
    )
    .bind(char_id)
    .fetch_all(pool)
    .await
}

/// Текущий XP-пул персонажа (вклад — списание пула, §2.1).
pub async fn xp_pool(pool: &PgPool, char_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT xp_pool FROM characters WHERE id = $1")
        .bind(char_id)
        .fetch_one(pool)
        .await
}

/// Вклад одной транзакцией (D27: «транзакционный флаш XP»): апсерт навыка
/// + списание пула. Атомарно — либо всё, либо ничего.
pub async fn invest_tx(
    pool: &PgPool,
    char_id: &str,
    skill_id: &str,
    xp: i64,
    rank: i32,
    pool_left: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO character_skills (character_id, skill_id, xp, rank) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (character_id, skill_id) DO UPDATE SET \
           xp = EXCLUDED.xp, rank = EXCLUDED.rank",
    )
    .bind(char_id)
    .bind(skill_id)
    .bind(xp)
    .bind(rank)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE characters SET xp_pool = $2 WHERE id = $1")
        .bind(char_id)
        .bind(pool_left)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

//! Крафт-джобы (M12.6, D27): ингредиенты списываются при старте, джоб
//! в БД — рестарт сервера не теряет материалы.

use sqlx::PgPool;

/// Рецепт из сида (payload `cmd/craft.recipes`, skills.md §5.4).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RecipeRow {
    pub id: String,
    pub name: String,
    pub profession: String,
    pub rank: i32,
    pub station: Option<String>,
    pub tool_type: Option<String>,
    pub requires: serde_json::Value,
    pub inputs: serde_json::Value,
    pub time_s: i32,
    pub xp_profession: i32,
    pub output_type: String,
    pub output_qty: i32,
    pub station_label: Option<String>,
}

/// Все рецепты (или одной профессии).
pub async fn recipes(
    pool: &PgPool,
    profession: Option<&str>,
) -> Result<Vec<RecipeRow>, sqlx::Error> {
    match profession {
        Some(p) => {
            sqlx::query_as::<_, RecipeRow>(
                "SELECT id, name, profession, rank, station, tool_type, requires, inputs, \
                 time_s, xp_profession, output_type, output_qty, station_label \
                 FROM recipes WHERE profession = $1 ORDER BY rank, id",
            )
            .bind(p)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, RecipeRow>(
                "SELECT id, name, profession, rank, station, tool_type, requires, inputs, \
                 time_s, xp_profession, output_type, output_qty, station_label \
                 FROM recipes ORDER BY profession, rank, id",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Один рецепт; `None` — неизвестный id.
pub async fn recipe(pool: &PgPool, recipe_id: &str) -> Result<Option<RecipeRow>, sqlx::Error> {
    sqlx::query_as::<_, RecipeRow>(
        "SELECT id, name, profession, rank, station, tool_type, requires, inputs, \
         time_s, xp_profession, output_type, output_qty, station_label \
         FROM recipes WHERE id = $1",
    )
    .bind(recipe_id)
    .fetch_optional(pool)
    .await
}

/// Активные джобы персонажа (для UI/проверки).
pub async fn active_jobs(pool: &PgPool, char_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM craft_jobs WHERE character_id = $1")
        .bind(char_id)
        .fetch_one(pool)
        .await
}

/// Создать джоб (ингредиенты уже списаны) → id.
pub async fn enqueue(
    pool: &PgPool,
    char_id: &str,
    recipe: &RecipeRow,
    qty: i64,
    finishes_at: f64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO craft_jobs \
           (character_id, recipe_id, qty, output_type, output_qty, finishes_at) \
         VALUES ($1, $2, $3, $4, $5, to_timestamp($6)) RETURNING id",
    )
    .bind(char_id)
    .bind(&recipe.id)
    .bind(qty)
    .bind(&recipe.output_type)
    .bind(recipe.output_qty)
    .bind(finishes_at)
    .fetch_one(pool)
    .await
}

/// Готовые джобы (finishes_at ≤ now) с данными выдачи.
pub async fn ready_jobs(
    pool: &PgPool,
    now: f64,
) -> Result<Vec<(i64, String, String, i64, i64)>, sqlx::Error> {
    // (id, character_id, recipe_id, total_output_qty, profession_xp)
    sqlx::query_as::<_, (i64, String, String, i64, i64)>(
        "SELECT j.id, j.character_id, j.recipe_id, \
                (j.output_qty::BIGINT) * j.qty, (r.xp_profession::BIGINT) * j.qty \
         FROM craft_jobs j JOIN recipes r ON r.id = j.recipe_id \
         WHERE j.finishes_at <= to_timestamp($1) ORDER BY j.id",
    )
    .bind(now)
    .fetch_all(pool)
    .await
}

/// Забрать готовый джоб; `false` — уже забран.
pub async fn take_job(pool: &PgPool, job_id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM craft_jobs WHERE id = $1")
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}

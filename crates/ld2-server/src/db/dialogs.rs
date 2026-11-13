//! M18/F35: кулдауны тайминг-строк диалогов (`every_s`): показ строки
//! (line_id) персонажем у NPC — epoch-секунды (см. `dialogs::visible_lines`).

use std::collections::HashMap;

use sqlx::PgPool;

/// Показы строк NPC: `line_id` → epoch-секунды последнего показа.
pub async fn shown_lines(
    pool: &PgPool,
    char_id: &str,
    npc_id: &str,
) -> Result<HashMap<String, i64>, sqlx::Error> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT line_id, shown_at FROM character_dialog_lines \
         WHERE character_id = $1 AND npc_id = $2",
    )
    .bind(char_id)
    .bind(npc_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Отметить показ строки (начало окна `every_s`).
pub async fn mark_shown(
    pool: &PgPool,
    char_id: &str,
    npc_id: &str,
    line_id: &str,
    now: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO character_dialog_lines (character_id, npc_id, line_id, shown_at) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (character_id, npc_id, line_id) \
         DO UPDATE SET shown_at = EXCLUDED.shown_at",
    )
    .bind(char_id)
    .bind(npc_id)
    .bind(line_id)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

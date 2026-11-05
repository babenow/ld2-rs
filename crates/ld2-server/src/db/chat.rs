//! История чата (ФР-13, M12.7): кольцевой буфер в БД — на каждой вставке
//! таблица обрезается до `server_history` строк ([gameplay.chat], D25).

use sqlx::PgPool;

/// Строка истории `(channel, from_char, name, text, sent_at_iso)`.
pub type ChatRow = (String, String, String, String, String);

/// Добавить сообщение и обрезать историю до `limit` последних строк.
/// Возвращает ISO-`ts` вставленной строки (`sent_at` из БД, единые часы).
pub async fn push(
    pool: &PgPool,
    channel: &str,
    from_char: &str,
    name: &str,
    text: &str,
    limit: u32,
) -> Result<String, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let sent_at: String = sqlx::query_scalar(
        "INSERT INTO chat_history (channel, from_char, name, text) \
         VALUES ($1, $2, $3, $4) \
         RETURNING to_char(sent_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
    )
    .bind(channel)
    .bind(from_char)
    .bind(name)
    .bind(text)
    .fetch_one(&mut *tx)
    .await?;
    // Обрезка кольца: оставляем `limit` последних строк (global-ring — как
    // в NFR §8: лимит общий на историю сервера).
    if limit > 0 {
        sqlx::query(
            "DELETE FROM chat_history WHERE id NOT IN \
             (SELECT id FROM chat_history ORDER BY id DESC LIMIT $1)",
        )
        .bind(limit as i64)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(sent_at)
}

/// Последние `limit` строк истории (старые первые) — для реплея при входе.
pub async fn recent(pool: &PgPool, limit: u32) -> Result<Vec<ChatRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatRow>(
        "SELECT channel, from_char, name, text, \
                to_char(sent_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') \
         FROM chat_history ORDER BY id DESC LIMIT $1",
    )
    .bind(limit as i64)
    .fetch_all(pool)
    .await
    .map(|mut rows| {
        rows.reverse(); // хронологический порядок
        rows
    })
}

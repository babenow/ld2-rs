//! Гильдии (ФР-16, M12.7d): write-through в БД + журнал `guild_log`.

use sqlx::PgPool;

/// Строка гильдии `(id, name)`.
pub type GuildRow = (String, String);

/// Участник `(character_id, name, rank, online)`; online считает вызывающий.
pub type MemberRow = (String, String, String);

/// Создать гильдию с лидером (одна транзакция) → id; `None` — имя занято.
pub async fn create(
    pool: &PgPool,
    name: &str,
    leader_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM guilds WHERE name = $1")
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_some() {
        return Ok(None);
    }
    let id: String = sqlx::query_scalar("INSERT INTO guilds (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO guild_members (guild_id, character_id, rank) \
         VALUES ($1, $2, 'leader')",
    )
    .bind(&id)
    .bind(leader_id)
    .execute(&mut *tx)
    .await?;
    log_tx(&mut tx, &id, "create", &format!("Гильдия «{name}» создана")).await?;
    tx.commit().await?;
    Ok(Some(id))
}

/// Гильдия по id `(id, name)`; `None` — не найдена.
pub async fn find(pool: &PgPool, guild_id: &str) -> Result<Option<GuildRow>, sqlx::Error> {
    sqlx::query_as::<_, GuildRow>("SELECT id, name FROM guilds WHERE id = $1")
        .bind(guild_id)
        .fetch_optional(pool)
        .await
}

/// Поиск гильдий (`guild.browse`, §7): подстрока имени, лимит 50.
pub async fn browse(pool: &PgPool, query: Option<&str>) -> Result<Vec<GuildRow>, sqlx::Error> {
    match query {
        Some(q) => {
            let pattern = format!("%{}%", q);
            sqlx::query_as::<_, GuildRow>(
                "SELECT id, name FROM guilds WHERE name ILIKE $1 ORDER BY name LIMIT 50",
            )
            .bind(pattern)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, GuildRow>("SELECT id, name FROM guilds ORDER BY name LIMIT 50")
                .fetch_all(pool)
                .await
        }
    }
}

/// Гильдия персонажа (устойчивый индекс: одна на персонажа — уникальный
/// индекс в схеме).
pub async fn guild_of(pool: &PgPool, char_id: &str) -> Result<Option<GuildRow>, sqlx::Error> {
    sqlx::query_as::<_, GuildRow>(
        "SELECT g.id, g.name FROM guild_members m \
         JOIN guilds g ON g.id = m.guild_id WHERE m.character_id = $1",
    )
    .bind(char_id)
    .fetch_optional(pool)
    .await
}

/// Участники гильдии `(character_id, name, rank)`.
pub async fn members(pool: &PgPool, guild_id: &str) -> Result<Vec<MemberRow>, sqlx::Error> {
    sqlx::query_as::<_, MemberRow>(
        "SELECT m.character_id, c.name, m.rank FROM guild_members m \
         JOIN characters c ON c.id = m.character_id \
         WHERE m.guild_id = $1 AND c.deleted_at IS NULL \
         ORDER BY CASE m.rank WHEN 'leader' THEN 0 WHEN 'officer' THEN 1 ELSE 2 END, c.name",
    )
    .bind(guild_id)
    .fetch_all(pool)
    .await
}

/// Ранг персонажа в гильдии; `None` — не состоит.
pub async fn rank_of(pool: &PgPool, char_id: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT rank FROM guild_members WHERE character_id = $1")
        .bind(char_id)
        .fetch_optional(pool)
        .await
}

/// Вступить (или присоединиться по приглашению) → false — уже состоит.
pub async fn join(
    pool: &PgPool,
    guild_id: &str,
    char_id: &str,
    rank: &str,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO guild_members (guild_id, character_id, rank) \
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(guild_id)
    .bind(char_id)
    .bind(rank)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    log_tx(
        &mut tx,
        guild_id,
        "join",
        "Новый участник вступил в гильдию",
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Выйти → false — не состоял.
pub async fn leave(pool: &PgPool, char_id: &str) -> Result<Option<String>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let guild_id: Option<String> =
        sqlx::query_scalar("DELETE FROM guild_members WHERE character_id = $1 RETURNING guild_id")
            .bind(char_id)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some(id) = &guild_id {
        log_tx(&mut tx, id, "leave", "Участник покинул гильдию").await?;
    }
    tx.commit().await?;
    Ok(guild_id)
}

/// Исключить участника (вызывающий уже проверен на права) → false — нет цели.
pub async fn kick(pool: &PgPool, char_id: &str) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let guild_id: Option<String> =
        sqlx::query_scalar("DELETE FROM guild_members WHERE character_id = $1 RETURNING guild_id")
            .bind(char_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(id) = guild_id else {
        return Ok(false);
    };
    log_tx(&mut tx, &id, "kick", "Участник исключён").await?;
    tx.commit().await?;
    Ok(true)
}

/// Смена ранга (атомарно, §7) → false — нет такого участника.
pub async fn set_rank(
    pool: &PgPool,
    guild_id: &str,
    char_id: &str,
    rank: &str,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let result = sqlx::query(
        "UPDATE guild_members SET rank = $3 \
         WHERE guild_id = $1 AND character_id = $2",
    )
    .bind(guild_id)
    .bind(char_id)
    .bind(rank)
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(false);
    }
    log_tx(
        &mut tx,
        guild_id,
        "rank_change",
        &format!("Ранг участника изменён на «{rank}»"),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Последние 50 записей журнала (новые первые) → `(kind, text, at_epoch)`.
pub async fn log_entries(
    pool: &PgPool,
    guild_id: &str,
) -> Result<Vec<(String, String, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (String, String, i64)>(
        "SELECT kind, text, EXTRACT(EPOCH FROM at)::BIGINT \
         FROM guild_log WHERE guild_id = $1 ORDER BY id DESC LIMIT 50",
    )
    .bind(guild_id)
    .fetch_all(pool)
    .await
    .map(|mut rows| {
        rows.reverse();
        rows
    })
}

/// Запись в журнал внутри существующей транзакции.
async fn log_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    guild_id: &str,
    kind: &str,
    text: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO guild_log (guild_id, kind, text) VALUES ($1, $2, $3)")
        .bind(guild_id)
        .bind(kind)
        .bind(text)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

//! Выдача предметов (стаки, типы, транзакции) (ФР-06/07).

use super::*;

pub async fn grant_stacking(
    pool: &PgPool,
    char_id: &str,
    def_id: &str,
    qty: i64,
) -> Result<(ItemRow, bool), sqlx::Error> {
    // M18/F58f: складываем только в «простой» стак (без состояния) —
    // инкрустированный/улучшенный экземпляр остаётся отдельной записью.
    let existing: Option<(String, i32)> = sqlx::query_as(
        "SELECT uid, qty FROM items WHERE character_id = $1 AND def_id = $2 \
         AND upgrade = 0 AND sockets = '[]'::jsonb \
         AND (bonuses IS NULL OR bonuses::text IN ('[]', 'null', '{}')) \
         ORDER BY created_at, uid LIMIT 1",
    )
    .bind(char_id)
    .bind(def_id)
    .fetch_optional(pool)
    .await?;
    if let Some((uid, cur)) = existing {
        let new_qty = i64::from(cur) + qty;
        sqlx::query("UPDATE items SET qty = $2 WHERE uid = $1")
            .bind(&uid)
            .bind(new_qty)
            .execute(pool)
            .await?;
        let row = find(pool, &uid, char_id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;
        return Ok((row, true));
    }
    Ok((insert(pool, char_id, def_id, qty).await?, false))
}

/// M18/F58f (новая фича): рюкзак вмещает **стаки**, а не предметы —
/// однотипное кладётся в существующий простой стак; если стака нет,
/// нужен свободный слот (`slots` — лимит из конфига). `None` —
/// инвентарь заполнен. `(row, stacked)`: `true` — прибавка в стак.
pub async fn grant_stacking_limited(
    pool: &PgPool,
    char_id: &str,
    def_id: &str,
    qty: i64,
    slots: i64,
) -> Result<Option<(ItemRow, bool)>, sqlx::Error> {
    if stackable_exists(pool, char_id, def_id).await? {
        return grant_stacking(pool, char_id, def_id, qty).await.map(Some);
    }
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM items WHERE character_id = $1")
        .bind(char_id)
        .fetch_one(pool)
        .await?;
    if n >= slots {
        return Ok(None);
    }
    insert(pool, char_id, def_id, qty)
        .await
        .map(|row| Some((row, false)))
}

/// F29a: выдать `qty` предметов первого определения типа `item_type`
/// (`/scenario gold`, награды): `None` — такого типа нет в контенте.
pub async fn grant_stacking_by_type(
    pool: &PgPool,
    char_id: &str,
    item_type: &str,
    qty: i64,
) -> Result<Option<(ItemRow, bool)>, sqlx::Error> {
    let def_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM item_defs WHERE item_type = $1 ORDER BY id LIMIT 1")
            .bind(item_type)
            .fetch_optional(pool)
            .await?;
    match def_id {
        Some(def_id) => grant_stacking(pool, char_id, &def_id, qty).await.map(Some),
        None => Ok(None),
    }
}

/// R6: grant_stacking на внешней транзакции (shop.buy).
pub async fn grant_stacking_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    char_id: &str,
    def_id: &str,
    qty: i64,
) -> Result<(ItemRow, bool), sqlx::Error> {
    // M18/F58f: складываем только в «простой» стак (без состояния).
    let existing: Option<(String, i32)> = sqlx::query_as(
        "SELECT uid, qty FROM items WHERE character_id = $1 AND def_id = $2 \
         AND upgrade = 0 AND sockets = '[]'::jsonb \
         AND (bonuses IS NULL OR bonuses::text IN ('[]', 'null', '{}')) \
         ORDER BY created_at, uid LIMIT 1",
    )
    .bind(char_id)
    .bind(def_id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((uid, cur)) = existing {
        let new_qty = i64::from(cur) + qty;
        sqlx::query("UPDATE items SET qty = $2 WHERE uid = $1")
            .bind(&uid)
            .bind(new_qty)
            .execute(&mut **tx)
            .await?;
        let row: ItemRow = sqlx::query_as(
            "SELECT uid, def_id, qty, upgrade, personal, equipped_slot, sockets, bonuses, container, slot_index \
             FROM items WHERE uid = $1",
        )
        .bind(&uid)
        .fetch_one(&mut **tx)
        .await?;
        return Ok((row, true));
    }
    let row: ItemRow = sqlx::query_as(
        "INSERT INTO items (character_id, def_id, qty, sort_order) \
         VALUES ($1, $2, $3, COALESCE((SELECT MAX(sort_order) + 1 \
         FROM items WHERE character_id = $1), 0)) \
         RETURNING uid, def_id, qty, upgrade, personal, equipped_slot, sockets, bonuses, container, slot_index",
    )
    .bind(char_id)
    .bind(def_id)
    .bind(qty)
    .fetch_one(&mut **tx)
    .await?;
    Ok((row, false))
}

/// Вставить предмет по типу или id определения (награды квестов и крафт,
/// M12.6): сначала точное совпадение `id`, затем первый `item_type`;
/// `None` — такого предмета нет в контенте.
pub async fn insert_by_type(
    pool: &PgPool,
    char_id: &str,
    item_type: &str,
    qty: i64,
) -> Result<Option<ItemRow>, sqlx::Error> {
    let by_id: Option<String> = sqlx::query_scalar("SELECT id FROM item_defs WHERE id = $1")
        .bind(item_type)
        .fetch_optional(pool)
        .await?;
    let def_id = match by_id {
        Some(id) => Some(id),
        None => {
            sqlx::query_scalar::<_, String>(
                "SELECT id FROM item_defs WHERE item_type = $1 ORDER BY id LIMIT 1",
            )
            .bind(item_type)
            .fetch_optional(pool)
            .await?
        }
    };
    let Some(def_id) = def_id else {
        return Ok(None);
    };
    // M18/F58f: квестовые награды и крафт — в существующий простой
    // стак (рюкзак вмещает стаки, а не предметы).
    grant_stacking(pool, char_id, &def_id, qty)
        .await
        .map(|(row, _)| Some(row))
}

/// R6: insert_by_type на внешней транзакции (награды квестов).
pub async fn insert_by_type_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    char_id: &str,
    item_type: &str,
    qty: i64,
) -> Result<Option<ItemRow>, sqlx::Error> {
    let by_id: Option<String> = sqlx::query_scalar("SELECT id FROM item_defs WHERE id = $1")
        .bind(item_type)
        .fetch_optional(&mut **tx)
        .await?;
    let def_id = match by_id {
        Some(id) => Some(id),
        None => {
            sqlx::query_scalar::<_, String>(
                "SELECT id FROM item_defs WHERE item_type = $1 ORDER BY id LIMIT 1",
            )
            .bind(item_type)
            .fetch_optional(&mut **tx)
            .await?
        }
    };
    let Some(def_id) = def_id else {
        return Ok(None);
    };
    grant_stacking_tx(tx, char_id, &def_id, qty)
        .await
        .map(|(row, _)| Some(row))
}

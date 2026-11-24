//! Изменения инстансов: вставка, экипировка, сокеты, заточка, лут (D27).

use super::*;

/// Новый инстанс в инвентаре (uid генерирует БД).
pub async fn insert(
    pool: &PgPool,
    char_id: &str,
    def_id: &str,
    qty: i64,
) -> Result<ItemRow, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "INSERT INTO items (character_id, def_id, qty, sort_order) \
         VALUES ($1, $2, $3, COALESCE((SELECT MAX(sort_order) + 1 \
         FROM items WHERE character_id = $1), 0)) \
         RETURNING uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses, container, slot_index",
    )
    .bind(char_id)
    .bind(def_id)
    .bind(qty)
    .fetch_one(pool)
    .await
}

/// R-dnd: подбор предмета с состоянием (заточка/камни/бонусы) —
/// отдельная запись с восстановлением колонок (в «простой» стак не
/// складывается — состояние не пережило бы склейку).
pub async fn insert_stateful(
    pool: &PgPool,
    char_id: &str,
    def_id: &str,
    qty: i64,
    upgrade: i32,
    sockets: &Value,
    bonuses: Option<Value>,
) -> Result<ItemRow, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "INSERT INTO items (character_id, def_id, qty, upgrade, sockets, bonuses, sort_order) \
         VALUES ($1, $2, $3, $4, $5, $6, COALESCE((SELECT MAX(sort_order) + 1 \
         FROM items WHERE character_id = $1), 0)) \
         RETURNING uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses, container, slot_index",
    )
    .bind(char_id)
    .bind(def_id)
    .bind(qty)
    .bind(upgrade)
    .bind(sockets)
    .bind(bonuses)
    .fetch_one(pool)
    .await
}

/// M18/F47: записать бонусы инстанса (ADR-010) — dev-сценарий
/// `/scenario bonus`, будущие источники в игре.
pub async fn set_bonuses(pool: &PgPool, uid: &str, bonuses: Value) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE items SET bonuses = $2 WHERE uid = $1")
        .bind(uid)
        .bind(bonuses)
        .execute(pool)
        .await?;
    Ok(())
}

/// Надеть/снять (slot = NULL — снять).
pub async fn set_equipped(pool: &PgPool, uid: &str, slot: Option<&str>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE items SET equipped_slot = $2 WHERE uid = $1")
        .bind(uid)
        .bind(slot)
        .execute(pool)
        .await?;
    Ok(())
}

/// Изменить количество стака (патч `evt/item/state`).
pub async fn set_qty(pool: &PgPool, uid: &str, qty: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE items SET qty = $2 WHERE uid = $1")
        .bind(uid)
        .bind(qty)
        .execute(pool)
        .await?;
    Ok(())
}

/// Полное удаление из инвентаря (`evt/item/removed`).
pub async fn delete(pool: &PgPool, uid: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("DELETE FROM items WHERE uid = $1")
        .bind(uid)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Удалить весь инвентарь (смерть, ФР-06) → строки для `lost_items`.
pub async fn delete_all(pool: &PgPool, char_id: &str) -> Result<Vec<ItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "DELETE FROM items WHERE character_id = $1 \
         RETURNING uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses, container, slot_index",
    )
    .bind(char_id)
    .fetch_all(pool)
    .await
}

/// Записать сокеты посоха (insert/extract рун, ADR-002).
pub async fn set_sockets(
    pool: &PgPool,
    uid: &str,
    sockets: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE items SET sockets = $2 WHERE uid = $1")
        .bind(uid)
        .bind(sockets)
        .execute(pool)
        .await?;
    Ok(())
}

/// M18/F58c: запись сокетов «если не изменились» (CAS) — тикер зарядов
/// читает-пишет инстанс и не должен затирать параллельный каст/вставку
/// того же предмета. `false` — значение ушло, читаем заново.
pub async fn set_sockets_if_unchanged(
    pool: &PgPool,
    uid: &str,
    expected: &serde_json::Value,
    new: serde_json::Value,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query("UPDATE items SET sockets = $3 WHERE uid = $1 AND sockets = $2")
        .bind(uid)
        .bind(expected)
        .bind(new)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Уровень улучшения инстанса (ФР-10; `upgrade` в §6 = def + инстанс).
pub async fn set_upgrade(pool: &PgPool, uid: &str, upgrade: i32) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE items SET upgrade = $2 WHERE uid = $1")
        .bind(uid)
        .bind(upgrade)
        .execute(pool)
        .await?;
    Ok(())
}

/// Взять лут одной транзакцией (D27): журнал взятого + инвентарь.
/// `None` — уже взят (→ `E_ITEM_GONE`); `Some(row)` — новый инстанс.
/// M18/F58f: подбор лута — однотипное в существующий простой стак
/// (рюкзак вмещает стаки), иначе новая запись. `(row, stacked)`:
/// `true` — прибавка в стак (qty обновлён), `false` — новая запись;
/// `None` — лут уже собран.
pub async fn take_loot_tx(
    pool: &PgPool,
    loot_uid: &str,
    char_id: &str,
    def_id: &str,
    qty: i64,
) -> Result<Option<(ItemRow, bool)>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let taken: Option<String> = sqlx::query_scalar(
        "INSERT INTO loot_taken (uid, character_id) VALUES ($1, $2) \
         ON CONFLICT (uid, character_id) DO NOTHING RETURNING uid",
    )
    .bind(loot_uid)
    .bind(char_id)
    .fetch_optional(&mut *tx)
    .await?;
    if taken.is_none() {
        return Ok(None); // tx откатывается при дропе
    }
    // Стак: та же селекция, что в grant_stacking (простая запись).
    let existing: Option<(String, i32)> = sqlx::query_as(
        "SELECT uid, qty FROM items WHERE character_id = $1 AND def_id = $2 \
         AND upgrade = 0 AND sockets = '[]'::jsonb \
         AND (bonuses IS NULL OR bonuses::text IN ('[]', 'null', '{}')) \
         ORDER BY created_at, uid LIMIT 1",
    )
    .bind(char_id)
    .bind(def_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((uid, cur)) = existing {
        sqlx::query("UPDATE items SET qty = $2 WHERE uid = $1")
            .bind(&uid)
            .bind(i64::from(cur) + qty)
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query_as::<_, ItemRow>(
            "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses, container, slot_index \
             FROM items WHERE uid = $1 AND character_id = $2",
        )
        .bind(&uid)
        .bind(char_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(Some((row, true)));
    }
    let row = sqlx::query_as::<_, ItemRow>(
        "INSERT INTO items (character_id, def_id, qty, sort_order) \
         VALUES ($1, $2, $3, COALESCE((SELECT MAX(sort_order) + 1 \
         FROM items WHERE character_id = $1), 0)) \
         RETURNING uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses, container, slot_index",
    )
    .bind(char_id)
    .bind(def_id)
    .bind(qty)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some((row, false)))
}

/// M16/F29g: поменять местами порядок двух предметов (перетаскивание
/// в рюкзаке); оба должны принадлежать персонажу. `false` — предмета
/// нет (ничего не изменено).
pub async fn swap_order(
    pool: &PgPool,
    char_id: &str,
    a: &str,
    b: &str,
) -> Result<bool, sqlx::Error> {
    if a == b {
        return Ok(true);
    }
    let mut tx = pool.begin().await?;
    let rows: Vec<(String, Option<i32>)> = sqlx::query_as(
        "SELECT uid, sort_order FROM items \
         WHERE character_id = $1 AND uid = ANY($2) FOR UPDATE",
    )
    .bind(char_id)
    .bind(vec![a.to_string(), b.to_string()])
    .fetch_all(&mut *tx)
    .await?;
    if rows.len() != 2 {
        return Ok(false); // tx откатывается
    }
    let so = |uid: &str| {
        rows.iter()
            .find(|(u, _)| u.as_str() == uid)
            .and_then(|(_, s)| *s)
    };
    let (so_a, so_b) = (so(a), so(b));
    sqlx::query("UPDATE items SET sort_order = $2 WHERE character_id = $1 AND uid = $3")
        .bind(char_id)
        .bind(so_b)
        .bind(a)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE items SET sort_order = $2 WHERE character_id = $1 AND uid = $3")
        .bind(char_id)
        .bind(so_a)
        .bind(b)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

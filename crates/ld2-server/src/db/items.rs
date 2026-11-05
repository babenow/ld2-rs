//! Предметы: инстансы инвентаря/экипировки и взятый статический лут
//! (M12.4, ФР-06/07, D27 — изменения персистятся немедленно).

use serde_json::Value;
use sqlx::PgPool;

/// Инстанс предмета в инвентаре персонажа.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ItemRow {
    pub uid: String,
    pub def_id: String,
    pub qty: i32,
    pub upgrade: i32,
    pub equipped_slot: Option<String>,
    /// Сокеты посоха: `[{index, rune?, def_id?}]` (ADR-002; `def_id` —
    /// служебное поле для извлечения, в §6 не попадает).
    pub sockets: serde_json::Value,
}

/// Определение из сида (`content/items.json` → `item_defs`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ItemDefRow {
    pub id: String,
    pub item_type: String,
    pub name: String,
    pub rarity: String,
    pub upgrade: i32,
    pub stats: Value,
    pub requires: Option<Value>,
    pub gold_value: i32,
    pub qty: i32,
    /// Количество сокетов посоха (ADR-002) — ленивая инициализация.
    pub sockets: i32,
}

/// Инвентарь персонажа в порядке получения (стабильный порядок UI).
pub async fn inventory(pool: &PgPool, char_id: &str) -> Result<Vec<ItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets FROM items \
         WHERE character_id = $1 ORDER BY created_at, uid",
    )
    .bind(char_id)
    .fetch_all(pool)
    .await
}

/// Определение предмета; `None` — нет в сиде.
pub async fn def(pool: &PgPool, def_id: &str) -> Result<Option<ItemDefRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemDefRow>(
        "SELECT id, item_type, name, rarity, upgrade, stats, requires, gold_value, qty, sockets \
         FROM item_defs WHERE id = $1",
    )
    .bind(def_id)
    .fetch_optional(pool)
    .await
}

/// Инстанс в инвентаре конкретного персонажа; `None` — чужой/несуществующий.
pub async fn find(pool: &PgPool, uid: &str, char_id: &str) -> Result<Option<ItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets FROM items \
         WHERE uid = $1 AND character_id = $2",
    )
    .bind(uid)
    .bind(char_id)
    .fetch_optional(pool)
    .await
}

/// Предмет, надетый в слоте (переэкипка снимает прежний).
pub async fn find_equipped_in_slot(
    pool: &PgPool,
    char_id: &str,
    slot: &str,
) -> Result<Option<ItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets FROM items \
         WHERE character_id = $1 AND equipped_slot = $2",
    )
    .bind(char_id)
    .bind(slot)
    .fetch_optional(pool)
    .await
}

/// Занятые слоты инвентаря (лимит из конфига, D25/DoD M2).
pub async fn count(pool: &PgPool, char_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM items WHERE character_id = $1")
        .bind(char_id)
        .fetch_one(pool)
        .await
}

/// Новый инстанс в инвентаре (uid генерирует БД).
pub async fn insert(
    pool: &PgPool,
    char_id: &str,
    def_id: &str,
    qty: i64,
) -> Result<ItemRow, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "INSERT INTO items (character_id, def_id, qty) VALUES ($1, $2, $3) \
         RETURNING uid, def_id, qty, upgrade, equipped_slot, sockets",
    )
    .bind(char_id)
    .bind(def_id)
    .bind(qty)
    .fetch_one(pool)
    .await
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
         RETURNING uid, def_id, qty, upgrade, equipped_slot, sockets",
    )
    .bind(char_id)
    .fetch_all(pool)
    .await
}

/// Взят ли персонажем статический лут (`loot:<entity>:<idx>` — однократно).
pub async fn loot_taken_exists(
    pool: &PgPool,
    loot_uid: &str,
    char_id: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM loot_taken WHERE uid = $1 AND character_id = $2)",
    )
    .bind(loot_uid)
    .bind(char_id)
    .fetch_one(pool)
    .await
}

/// Взять лут одной транзакцией (D27): журнал взятого + инвентарь.
/// `None` — уже взят (→ `E_ITEM_GONE`); `Some(row)` — новый инстанс.
pub async fn take_loot_tx(
    pool: &PgPool,
    loot_uid: &str,
    char_id: &str,
    def_id: &str,
    qty: i64,
) -> Result<Option<ItemRow>, sqlx::Error> {
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
    let row = sqlx::query_as::<_, ItemRow>(
        "INSERT INTO items (character_id, def_id, qty) VALUES ($1, $2, $3) \
         RETURNING uid, def_id, qty, upgrade, equipped_slot, sockets",
    )
    .bind(char_id)
    .bind(def_id)
    .bind(qty)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(row))
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

//! Запросы инвентаря и предметов (M12.4, ФР-06/07).

use super::*;

/// Инвентарь персонажа в порядке получения (стабильный порядок UI).
pub async fn inventory(pool: &PgPool, char_id: &str) -> Result<Vec<ItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses, container, slot_index FROM items \
         WHERE character_id = $1 ORDER BY sort_order NULLS LAST, created_at, uid",
    )
    .bind(char_id)
    .fetch_all(pool)
    .await
}

/// M18/F58c (ADR-012 §4): предметы с непустыми сокетами у указанных
/// персонажей — тикер зарядов рун; `character_id` идёт первой колонкой
/// (адресация патча `evt/item/state` владельцу).
pub async fn with_sockets(
    pool: &PgPool,
    char_ids: &[String],
) -> Result<Vec<(String, ItemRow)>, sqlx::Error> {
    if char_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            i32,
            i32,
            Option<String>,
            Value,
            Option<Value>,
            String,
            Option<i32>,
        ),
    >(
        "SELECT character_id, uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses, container, slot_index \
         FROM items WHERE character_id = ANY($1) AND sockets <> '[]'",
    )
    .bind(char_ids)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                char_id,
                uid,
                def_id,
                qty,
                upgrade,
                equipped_slot,
                sockets,
                bonuses,
                container,
                slot_index,
            )| {
                (
                    char_id,
                    ItemRow {
                        uid,
                        def_id,
                        qty,
                        upgrade,
                        equipped_slot,
                        sockets,
                        bonuses,
                        container,
                        slot_index,
                    },
                )
            },
        )
        .collect())
}

/// Определение предмета; `None` — нет в сиде.
pub async fn def(pool: &PgPool, def_id: &str) -> Result<Option<ItemDefRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemDefRow>(
        "SELECT id, item_type, name, rarity, upgrade, stats, requires, gold_value, qty, sockets, \
                rune_sockets, slot, hands, skill, cooldown_s, bonuses, bag_slots \
         FROM item_defs WHERE id = $1",
    )
    .bind(def_id)
    .fetch_optional(pool)
    .await
}

pub async fn admin_inventory(
    pool: &PgPool,
    char_id: &str,
) -> Result<Vec<AdminInvRow>, sqlx::Error> {
    sqlx::query_as::<_, AdminInvRow>(
        "SELECT i.uid, i.def_id, i.qty, d.name, d.item_type, d.rarity \
         FROM items i JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 ORDER BY i.created_at, i.uid",
    )
    .bind(char_id)
    .fetch_all(pool)
    .await
}

/// M24: каталог всех определений (админ-панель, вариант A — одним
/// запросом); порядок — по типу и имени.
pub async fn defs_all(pool: &PgPool) -> Result<Vec<ItemDefRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemDefRow>(
        "SELECT id, item_type, name, rarity, upgrade, stats, requires, gold_value, qty, sockets, \
                rune_sockets, slot, hands, skill, cooldown_s, bonuses, bag_slots \
         FROM item_defs ORDER BY item_type, name",
    )
    .fetch_all(pool)
    .await
}

/// Инстанс в инвентаре конкретного персонажа; `None` — чужой/несуществующий.
pub async fn find(pool: &PgPool, uid: &str, char_id: &str) -> Result<Option<ItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses, container, slot_index FROM items \
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
        "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses, container, slot_index FROM items \
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

/// Строка предмета по uid **без** проверки владельца (сделки: предмет
/// ещё у стороны-владельца, но читаем для зеркала §8).
pub async fn find_any(pool: &PgPool, uid: &str) -> Result<Option<ItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses, container, slot_index FROM items WHERE uid = $1",
    )
    .bind(uid)
    .fetch_optional(pool)
    .await
}

/// F29a: выдать `qty` в существующий стак `def_id` (самый старый)
/// либо новым стаком — монеты при подборе/начислении не съедают
/// слоты. `true` — добавлено в существий стак (клиенту патч `qty`).
/// M18/F58f: есть ли «простой» стак предмета (для решения «нужен ли
/// свободный слот» — стак не занимает место).
pub async fn stackable_exists(
    pool: &PgPool,
    char_id: &str,
    def_id: &str,
) -> Result<bool, sqlx::Error> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM items WHERE character_id = $1 AND def_id = $2 \
         AND upgrade = 0 AND sockets = '[]'::jsonb \
         AND (bonuses IS NULL OR bonuses::text IN ('[]', 'null', '{}'))",
    )
    .bind(char_id)
    .bind(def_id)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
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

/// M20/F20b: есть ли у персонажа предмет с этим `item_type`
/// (паритет проверки инструмента крафта).
pub async fn has_item_type(
    pool: &PgPool,
    char_id: &str,
    item_type: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM items i JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 AND d.item_type = $2)",
    )
    .bind(char_id)
    .bind(item_type)
    .fetch_one(pool)
    .await
}

/// Есть ли в инвентаре предмет с данным `item_type` (инструмент, M12.6).
pub async fn inventory_has_type(
    pool: &PgPool,
    char_id: &str,
    item_type: &str,
) -> Result<bool, sqlx::Error> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM items i JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 AND d.item_type = $2",
    )
    .bind(char_id)
    .bind(item_type)
    .fetch_one(pool)
    .await?;
    Ok(n > 0)
}

/// Сколько штук (сумма стаков) предметов данного `item_type`.
pub async fn count_by_type(
    pool: &PgPool,
    char_id: &str,
    item_type: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(i.qty), 0) FROM items i JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 AND d.item_type = $2",
    )
    .bind(char_id)
    .bind(item_type)
    .fetch_one(pool)
    .await
}

/// M20: сброс статического лута сущности (респавн) — все персонажи
/// могут взять лут заново. Сравнение по префиксу `loot:<entity>:`
/// (без LIKE — `_` в id не должен быть wildcard).
pub async fn loot_taken_reset(pool: &PgPool, entity_id: &str) -> Result<u64, sqlx::Error> {
    let prefix = format!("loot:{entity_id}:");
    let res = sqlx::query("DELETE FROM loot_taken WHERE left(uid, length($1)) = $1")
        .bind(&prefix)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

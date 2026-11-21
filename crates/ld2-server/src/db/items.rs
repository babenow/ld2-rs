//! Предметы: инстансы инвентаря/экипировки и взятый статический лут
//! (M12.4, ФР-06/07, D27 — изменения персистятся немедленно).

use serde_json::Value;
use sqlx::PgPool;

/// F29a (M16): деньги — предметы-монеты (тип контента; стак в
/// инвентаре, лут с мобов, оплата покупок).
pub const MONEY_TYPE: &str = "money";

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
    /// M18/F47: бонусы инстанса (ADR-010); `NULL` — нет своих бонусов.
    pub bonuses: Option<Value>,
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
    /// M18/F58 (ADR-012, уточнение заказчика): сколько слотов
    /// принимают руны; `NULL` — не указано → дефолт по типу предмета.
    pub rune_sockets: Option<i32>,
    /// M17/F30b: слот предмета (main_hand/off_hand/head/…; ring/artifact —
    /// псевдослоты пар); `None` — не экипируется.
    pub slot: Option<String>,
    /// M17/F30b: руки (2 — двуручник: занимает обе, блокирует левую).
    pub hands: i32,
    /// Навык оружия (skills.md §2.3; идёт в снапшот `extra["skill"]`).
    pub skill: Option<String>,
    /// M17/F30g: кулдаун использования/скорость атаки, с (контент,
    /// дефолт 1.0).
    pub cooldown_s: f64,
    /// M18/F47: бонусы из контента (ADR-010); `NULL` — их нет.
    pub bonuses: Option<Value>,
}

/// Инвентарь персонажа в порядке получения (стабильный порядок UI).
pub async fn inventory(pool: &PgPool, char_id: &str) -> Result<Vec<ItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses FROM items \
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
        ),
    >(
        "SELECT character_id, uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses \
         FROM items WHERE character_id = ANY($1) AND sockets <> '[]'",
    )
    .bind(char_ids)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(char_id, uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses)| {
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
                rune_sockets, slot, hands, skill, cooldown_s, bonuses \
         FROM item_defs WHERE id = $1",
    )
    .bind(def_id)
    .fetch_optional(pool)
    .await
}

/// M24: инвентарь персонажа с данными определений (правая колонка
/// админ-окна): uid/деф/количество + имя/тип/редкость одним запросом.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AdminInvRow {
    pub uid: String,
    pub def_id: String,
    pub qty: i32,
    pub name: String,
    pub item_type: String,
    pub rarity: String,
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
                rune_sockets, slot, hands, skill, cooldown_s, bonuses \
         FROM item_defs ORDER BY item_type, name",
    )
    .fetch_all(pool)
    .await
}

/// Инстанс в инвентаре конкретного персонажа; `None` — чужой/несуществующий.
pub async fn find(pool: &PgPool, uid: &str, char_id: &str) -> Result<Option<ItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses FROM items \
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
        "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses FROM items \
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
        "INSERT INTO items (character_id, def_id, qty, sort_order) \
         VALUES ($1, $2, $3, COALESCE((SELECT MAX(sort_order) + 1 \
         FROM items WHERE character_id = $1), 0)) \
         RETURNING uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses",
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
         RETURNING uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses",
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
         RETURNING uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses",
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
            "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses \
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
         RETURNING uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses",
    )
    .bind(char_id)
    .bind(def_id)
    .bind(qty)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some((row, false)))
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

/// Списать `need` штук по `item_type` (стаки убывают, пустые — удаляются);
/// `false` — не хватает (атомарно не защищено — проверка вызывающим).
pub async fn consume_by_type(
    pool: &PgPool,
    char_id: &str,
    item_type: &str,
    need: i64,
) -> Result<bool, sqlx::Error> {
    let rows: Vec<(String, i32)> = sqlx::query_as(
        "SELECT i.uid, i.qty FROM items i JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 AND d.item_type = $2 ORDER BY i.created_at, i.uid",
    )
    .bind(char_id)
    .bind(item_type)
    .fetch_all(pool)
    .await?;
    let mut left = need;
    for (uid, qty) in rows {
        if left <= 0 {
            break;
        }
        let take = i64::from(qty).min(left);
        left -= take;
        let rest = qty - take as i32;
        if rest <= 0 {
            sqlx::query("DELETE FROM items WHERE uid = $1")
                .bind(&uid)
                .execute(pool)
                .await?;
        } else {
            set_qty(pool, &uid, i64::from(rest)).await?;
        }
    }
    Ok(left <= 0)
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

/// Строка предмета по uid **без** проверки владельца (сделки: предмет
/// ещё у стороны-владельца, но читаем для зеркала §8).
pub async fn find_any(pool: &PgPool, uid: &str) -> Result<Option<ItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ItemRow>(
        "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses FROM items WHERE uid = $1",
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

/// M18/F39: списать `need` единиц `def_id` (валюта магазина) со счётом
/// стаков — семантика `consume_money`: `None` — не хватает (ничего не
/// списано); `(uid, новое qty | None = стак удалён)` для событий.
pub async fn consume_by_def(
    pool: &PgPool,
    char_id: &str,
    def_id: &str,
    need: i64,
) -> Result<Option<Vec<(String, Option<i64>)>>, sqlx::Error> {
    if need <= 0 {
        return Ok(Some(Vec::new()));
    }
    let have: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(qty), 0) FROM items WHERE character_id = $1 AND def_id = $2",
    )
    .bind(char_id)
    .bind(def_id)
    .fetch_one(pool)
    .await?;
    if have < need {
        return Ok(None);
    }
    let rows: Vec<(String, i32)> = sqlx::query_as(
        "SELECT uid, qty FROM items WHERE character_id = $1 AND def_id = $2 \
         ORDER BY created_at, uid",
    )
    .bind(char_id)
    .bind(def_id)
    .fetch_all(pool)
    .await?;
    let mut left = need;
    let mut changes = Vec::new();
    for (uid, qty) in rows {
        if left <= 0 {
            break;
        }
        let take = i64::from(qty).min(left);
        left -= take;
        let rest = qty - take as i32;
        if rest <= 0 {
            sqlx::query("DELETE FROM items WHERE uid = $1")
                .bind(&uid)
                .execute(pool)
                .await?;
            changes.push((uid, None));
        } else {
            set_qty(pool, &uid, i64::from(rest)).await?;
            changes.push((uid, Some(i64::from(rest))));
        }
    }
    Ok(Some(changes))
}

/// F29a: списать `need` монет со счётом стаков (пустые удаляются) —
/// `(uid, новое qty | None = стак удалён)` для `evt/item/state`/
/// `evt/item/removed`. `None` — не хватает (ничего не списано);
/// баланс проверяется здесь же — вызывающий не теряет монеты.
pub async fn consume_money(
    pool: &PgPool,
    char_id: &str,
    need: i64,
) -> Result<Option<Vec<(String, Option<i64>)>>, sqlx::Error> {
    if need <= 0 {
        return Ok(Some(Vec::new()));
    }
    if count_by_type(pool, char_id, MONEY_TYPE).await? < need {
        return Ok(None);
    }
    let rows: Vec<(String, i32)> = sqlx::query_as(
        "SELECT i.uid, i.qty FROM items i \
         JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 AND d.item_type = $2 \
         ORDER BY i.created_at, i.uid",
    )
    .bind(char_id)
    .bind(MONEY_TYPE)
    .fetch_all(pool)
    .await?;
    let mut left = need;
    let mut changes = Vec::new();
    for (uid, qty) in rows {
        if left <= 0 {
            break;
        }
        let take = i64::from(qty).min(left);
        left -= take;
        let rest = qty - take as i32;
        if rest <= 0 {
            sqlx::query("DELETE FROM items WHERE uid = $1")
                .bind(&uid)
                .execute(pool)
                .await?;
            changes.push((uid, None));
        } else {
            set_qty(pool, &uid, i64::from(rest)).await?;
            changes.push((uid, Some(i64::from(rest))));
        }
    }
    Ok(Some(changes))
}

// ═══ R6 (REVIEW-M25): ядра на переданной транзакции — покупки,
// награды квестов и обучение исполняются одной транзакцией
// (эталон — trade_execute); ошибка любого шага откатывает всё. ═══

/// R6: consume_by_def на внешней транзакции (shop.buy, skill.train).
pub async fn consume_by_def_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    char_id: &str,
    def_id: &str,
    need: i64,
) -> Result<Option<Vec<(String, Option<i64>)>>, sqlx::Error> {
    if need <= 0 {
        return Ok(Some(Vec::new()));
    }
    let have: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(qty), 0) FROM items WHERE character_id = $1 AND def_id = $2",
    )
    .bind(char_id)
    .bind(def_id)
    .fetch_one(&mut **tx)
    .await?;
    if have < need {
        return Ok(None);
    }
    let rows: Vec<(String, i32)> = sqlx::query_as(
        "SELECT uid, qty FROM items WHERE character_id = $1 AND def_id = $2 \
         ORDER BY created_at, uid",
    )
    .bind(char_id)
    .bind(def_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut left = need;
    let mut changes = Vec::new();
    for (uid, qty) in rows {
        if left <= 0 {
            break;
        }
        let take = i64::from(qty).min(left);
        left -= take;
        let rest = qty - take as i32;
        if rest <= 0 {
            sqlx::query("DELETE FROM items WHERE uid = $1")
                .bind(&uid)
                .execute(&mut **tx)
                .await?;
            changes.push((uid, None));
        } else {
            sqlx::query("UPDATE items SET qty = $2 WHERE uid = $1")
                .bind(&uid)
                .bind(i64::from(rest))
                .execute(&mut **tx)
                .await?;
            changes.push((uid, Some(i64::from(rest))));
        }
    }
    Ok(Some(changes))
}

/// R6: consume_money на внешней транзакции (skill.train).
pub async fn consume_money_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    char_id: &str,
    need: i64,
) -> Result<Option<Vec<(String, Option<i64>)>>, sqlx::Error> {
    if need <= 0 {
        return Ok(Some(Vec::new()));
    }
    let have: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(i.qty), 0) FROM items i \
         JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 AND d.item_type = $2",
    )
    .bind(char_id)
    .bind(MONEY_TYPE)
    .fetch_one(&mut **tx)
    .await?;
    if have < need {
        return Ok(None);
    }
    let rows: Vec<(String, i32)> = sqlx::query_as(
        "SELECT i.uid, i.qty FROM items i \
         JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 AND d.item_type = $2 \
         ORDER BY i.created_at, i.uid",
    )
    .bind(char_id)
    .bind(MONEY_TYPE)
    .fetch_all(&mut **tx)
    .await?;
    let mut left = need;
    let mut changes = Vec::new();
    for (uid, qty) in rows {
        if left <= 0 {
            break;
        }
        let take = i64::from(qty).min(left);
        left -= take;
        let rest = qty - take as i32;
        if rest <= 0 {
            sqlx::query("DELETE FROM items WHERE uid = $1")
                .bind(&uid)
                .execute(&mut **tx)
                .await?;
            changes.push((uid, None));
        } else {
            sqlx::query("UPDATE items SET qty = $2 WHERE uid = $1")
                .bind(&uid)
                .bind(i64::from(rest))
                .execute(&mut **tx)
                .await?;
            changes.push((uid, Some(i64::from(rest))));
        }
    }
    Ok(Some(changes))
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
            "SELECT uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses \
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
         RETURNING uid, def_id, qty, upgrade, equipped_slot, sockets, bonuses",
    )
    .bind(char_id)
    .bind(def_id)
    .bind(qty)
    .fetch_one(&mut **tx)
    .await?;
    Ok((row, false))
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

//! M28/B2: контейнеры инвентаря — рюкзак и надетые сумки.
//!
//! Контейнеры: `backpack` (база из конфига), `bags` (слоты под сумки) и
//! `<uid сумки>` (содержимое надетой сумки). Сумки дают ёмкость, а сами
//! занимают слоты `bags`, поэтому в «занято» не считаются.

use sqlx::PgPool;

/// Контейнер рюкзака (базовые ячейки персонажа).
pub const BACKPACK: &str = "backpack";
/// Слоты под надетые сумки.
pub const BAGS: &str = "bags";
/// M28: определение стартовой сумки (content/items.json).
pub const STARTER_BAG_DEF: &str = "traveler_bag";

/// Есть ли предметы внутри сумки `uid` (снять/выбросить непустую нельзя).
pub async fn bag_has_contents(pool: &PgPool, uid: &str) -> Result<bool, sqlx::Error> {
    let found: Option<String> =
        sqlx::query_scalar("SELECT uid FROM items WHERE container = $1 LIMIT 1")
            .bind(uid)
            .fetch_optional(pool)
            .await?;
    Ok(found.is_some())
}

/// Ёмкость: база из конфига + `bag_slots` всех надетых сумок.
pub async fn capacity(pool: &PgPool, char_id: &str, base: i64) -> Result<i64, sqlx::Error> {
    let extra: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(d.bag_slots), 0) FROM items i \
         JOIN item_defs d ON d.id = i.def_id \
         WHERE i.character_id = $1 AND i.container = 'bags' AND i.equipped_slot IS NULL",
    )
    .bind(char_id)
    .fetch_one(pool)
    .await?;
    Ok(base + extra)
}

/// Занято ячеек: всё не надетое, кроме самих сумок (они дают ёмкость).
pub async fn used(pool: &PgPool, char_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM items WHERE character_id = $1 \
         AND equipped_slot IS NULL AND container <> 'bags'",
    )
    .bind(char_id)
    .fetch_one(pool)
    .await
}

/// Есть ли место под новый предмет (с учётом надетых сумок).
pub async fn has_space(pool: &PgPool, char_id: &str, base: i64) -> Result<bool, sqlx::Error> {
    Ok(used(pool, char_id).await? < capacity(pool, char_id, base).await?)
}

/// Ошибка перемещения (маппится в коды протокола в `session/items.rs`).
#[derive(Debug, PartialEq, Eq)]
pub enum PlaceError {
    /// Предмета нет у персонажа.
    Gone,
    /// Предмет надет — двигать нельзя (сначала снять).
    Equipped,
    /// Нет такой надетой сумки.
    NoSuchBag,
    /// В `bags` кладут не сумку.
    NotABag,
    /// Сумка не пуста — снять нельзя.
    BagNotEmpty,
    /// Сумку в сумку класть нельзя.
    BagInBag,
    /// Индекс вне ёмкости контейнера.
    OutOfRange,
    /// Слот занят другой сумкой (клиент сначала освобождает).
    BagBusy,
    /// Сумка не принимает этот тип предмета (`bag_types`).
    WrongType,
    /// Внутренняя ошибка БД.
    Internal,
}

/// Тип предмета (контент `type`) по uid — для проверки `bag_types`.
async fn item_type(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    char_id: &str,
    uid: &str,
) -> Result<Option<String>, PlaceError> {
    sqlx::query_scalar(
        "SELECT d.item_type FROM items i JOIN item_defs d ON d.id = i.def_id \
         WHERE i.uid = $1 AND i.character_id = $2",
    )
    .bind(uid)
    .bind(char_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| PlaceError::Internal)
}

/// Принимает ли контейнер тип предмета: рюкзак и слоты сумок — любые,
/// сумка — по списку `bag_types` (пусто — любые).
async fn container_accepts(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    char_id: &str,
    container: &str,
    item_type: &str,
) -> Result<bool, PlaceError> {
    if container == BACKPACK || container == BAGS {
        return Ok(true);
    }
    let types: Option<Option<Vec<String>>> = sqlx::query_scalar(
        "SELECT d.bag_types FROM items b JOIN item_defs d ON d.id = b.def_id \
         WHERE b.uid = $1 AND b.character_id = $2 AND b.container = 'bags'",
    )
    .bind(container)
    .bind(char_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| PlaceError::Internal)?;
    Ok(match types {
        None => false,
        Some(None) => true,
        Some(Some(list)) => list.is_empty() || list.iter().any(|t| t == item_type),
    })
}

/// Переместить предмет в контейнер `to_container` на позицию `to_index`.
///
/// `base` — базовая ёмкость рюкзака, `bag_slots` — число слотов под
/// сумки из конфига. Если целевая ячейка занята, предметы меняются
/// местами (кроме слота `bags`, где занятость — ошибка `BagBusy`).
pub async fn place(
    pool: &PgPool,
    char_id: &str,
    uid: &str,
    to_container: &str,
    to_index: i32,
    base: i64,
    bag_slots: i32,
) -> Result<(), PlaceError> {
    if to_index < 0 {
        return Err(PlaceError::OutOfRange);
    }
    let mut tx = pool.begin().await.map_err(|_| PlaceError::Internal)?;
    let moving: Option<(String, String, Option<i32>, Option<String>)> = sqlx::query_as(
        "SELECT def_id, container, slot_index, equipped_slot FROM items \
         WHERE uid = $1 AND character_id = $2 FOR UPDATE",
    )
    .bind(uid)
    .bind(char_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| PlaceError::Internal)?;
    let Some((def_id, from_container, from_index, equipped)) = moving else {
        return Err(PlaceError::Gone);
    };
    if equipped.is_some() {
        return Err(PlaceError::Equipped);
    }
    let moving_bag: Option<i32> =
        sqlx::query_scalar("SELECT bag_slots FROM item_defs WHERE id = $1")
            .bind(&def_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| PlaceError::Internal)?;

    // Та же ячейка — нечего делать.
    if from_container == to_container && from_index == Some(to_index) {
        tx.commit().await.map_err(|_| PlaceError::Internal)?;
        return Ok(());
    }

    // Снятие сумки: она должна быть пуста.
    let leaving_bags = from_container == BAGS && to_container != BAGS;
    if leaving_bags {
        let filled: Option<String> = sqlx::query_scalar(
            "SELECT uid FROM items WHERE container = $1 AND character_id = $2 LIMIT 1",
        )
        .bind(uid)
        .bind(char_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| PlaceError::Internal)?;
        if filled.is_some() {
            return Err(PlaceError::BagNotEmpty);
        }
    }

    let target_cap: i32 = if to_container == BAGS {
        if moving_bag.is_none() {
            return Err(PlaceError::NotABag);
        }
        bag_slots
    } else if to_container == BACKPACK {
        i32::try_from(base).unwrap_or(i32::MAX)
    } else {
        if moving_bag.is_some() {
            return Err(PlaceError::BagInBag);
        }
        let cap: Option<i32> = sqlx::query_scalar(
            "SELECT d.bag_slots FROM items b JOIN item_defs d ON d.id = b.def_id \
             WHERE b.uid = $1 AND b.character_id = $2 AND b.container = 'bags' \
               AND b.equipped_slot IS NULL",
        )
        .bind(to_container)
        .bind(char_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| PlaceError::Internal)?;
        cap.ok_or(PlaceError::NoSuchBag)?
    };
    if to_index >= target_cap {
        return Err(PlaceError::OutOfRange);
    }

    let occupant: Option<String> = sqlx::query_scalar(
        "SELECT uid FROM items WHERE character_id = $1 AND container = $2 \
         AND slot_index = $3 AND uid <> $4 FOR UPDATE",
    )
    .bind(char_id)
    .bind(to_container)
    .bind(to_index)
    .bind(uid)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| PlaceError::Internal)?;
    if occupant.is_some() && to_container == BAGS {
        return Err(PlaceError::BagBusy);
    }

    // M28: у сумки может быть список принимаемых типов (`bag_types`);
    // при свопе вытесняемый предмет уезжает в исходный контейнер —
    // он тоже должен его принимать.
    let moving_type = item_type(&mut tx, char_id, uid)
        .await?
        .ok_or(PlaceError::Gone)?;
    if !container_accepts(&mut tx, char_id, to_container, &moving_type).await? {
        return Err(PlaceError::WrongType);
    }
    if let Some(occ) = &occupant {
        let occ_type = item_type(&mut tx, char_id, occ)
            .await?
            .ok_or(PlaceError::Gone)?;
        if !container_accepts(&mut tx, char_id, &from_container, &occ_type).await? {
            return Err(PlaceError::WrongType);
        }
    }

    // Освобождаем текущую позицию (уникальный индекс не терпит дублей).
    sqlx::query("UPDATE items SET container = '__moving__', slot_index = NULL WHERE uid = $1")
        .bind(uid)
        .execute(&mut *tx)
        .await
        .map_err(|_| PlaceError::Internal)?;
    if let Some(occ) = &occupant {
        sqlx::query("UPDATE items SET container = $2, slot_index = $3 WHERE uid = $1")
            .bind(occ)
            .bind(&from_container)
            .bind(from_index)
            .execute(&mut *tx)
            .await
            .map_err(|_| PlaceError::Internal)?;
    }
    sqlx::query("UPDATE items SET container = $2, slot_index = $3 WHERE uid = $1")
        .bind(uid)
        .bind(to_container)
        .bind(to_index)
        .execute(&mut *tx)
        .await
        .map_err(|_| PlaceError::Internal)?;
    tx.commit().await.map_err(|_| PlaceError::Internal)?;
    Ok(())
}

/// Выдать стартовый набор сумок персонажу (новый персонаж, B2).
/// Возвращает число выданных сумок; `0` — определения нет в контенте.
pub async fn grant_bags(
    pool: &PgPool,
    char_id: &str,
    count: i32,
    def_id: &str,
) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM item_defs WHERE id = $1")
        .bind(def_id)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Ok(0);
    }
    let mut granted = 0;
    for slot in 0..count {
        let n = sqlx::query(
            "INSERT INTO items (character_id, def_id, qty, container, slot_index, sort_order) \
             VALUES ($1, $2, 1, 'bags', $3, 0) ON CONFLICT DO NOTHING",
        )
        .bind(char_id)
        .bind(def_id)
        .bind(slot)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        granted += n;
    }
    sqlx::query("UPDATE characters SET bags_granted = TRUE WHERE id = $1")
        .bind(char_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(granted)
}

/// Догнать существующих персонажей (B2): тем, кому сумки ещё не
/// выдавались, кладём стартовый набор. Идемпотентно.
pub async fn ensure_bags(pool: &PgPool, count: i32, def_id: &str) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM item_defs WHERE id = $1")
        .bind(def_id)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Ok(0);
    }
    let ids: Vec<String> = sqlx::query_scalar(
        "UPDATE characters SET bags_granted = TRUE \
         WHERE NOT bags_granted AND deleted_at IS NULL \
           AND NOT EXISTS (SELECT 1 FROM items i \
                           WHERE i.character_id = characters.id AND i.container = 'bags') \
         RETURNING id",
    )
    .fetch_all(&mut *tx)
    .await?;
    for id in &ids {
        for slot in 0..count {
            sqlx::query(
                "INSERT INTO items (character_id, def_id, qty, container, slot_index, sort_order) \
                 VALUES ($1, $2, 1, 'bags', $3, 0)",
            )
            .bind(id)
            .bind(def_id)
            .bind(slot)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(ids.len() as u64)
}

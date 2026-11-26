//! Списание предметов и денег (магазины, крафт, оплаты) (ФР-06/07).

use super::*;

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

//! Локации мира: чтение из сида (D26), спавн, персист позиции персонажа
//! (критичное событие D27 — флаш при каждом переходе).

use serde_json::Value;
use sqlx::PgPool;

use crate::content::ContentExit;
use ld2_protocol::types::{Exit, LocationSnapshot};

/// Строка `locations` (миграция 0004).
#[derive(Debug, sqlx::FromRow)]
pub struct LocationRow {
    pub id: String,
    pub name: String,
    pub flags: Vec<String>,
    pub text: String,
    pub exits: Value,
    pub entities: Value,
}

impl LocationRow {
    /// Проводной снапшот §6: выходы — без `to` (живёт только в БД/контенте).
    pub fn snapshot(&self) -> LocationSnapshot {
        let exits: Vec<ContentExit> =
            serde_json::from_value(self.exits.clone()).unwrap_or_default();
        LocationSnapshot {
            id: self.id.clone(),
            name: self.name.clone(),
            flags: self.flags.clone(),
            text: self.text.clone(),
            exits: exits.iter().map(ContentExit::to_wire).collect(),
            entities: serde_json::from_value(self.entities.clone()).unwrap_or_default(),
            extra: Default::default(),
        }
    }

    /// Выход с данным id и его целевая локация (для `location.move`).
    pub fn exit_target(&self, exit_id: &str) -> Option<(Exit, String)> {
        let exits: Vec<ContentExit> = serde_json::from_value(self.exits.clone()).ok()?;
        exits
            .into_iter()
            .find(|e| e.id == exit_id)
            .map(|e| (e.to_wire(), e.to))
    }
}

/// Локация по id; `None` — нет в сиде (удалена/не засеяна).
pub async fn get(pool: &PgPool, id: &str) -> Result<Option<LocationRow>, sqlx::Error> {
    sqlx::query_as::<_, LocationRow>(
        "SELECT id, name, flags, text, exits, entities FROM locations WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Имя локации для сводок §5; `None` — неизвестна.
pub async fn location_name(pool: &PgPool, id: &str) -> Result<Option<String>, sqlx::Error> {
    if id.is_empty() {
        return Ok(None);
    }
    sqlx::query_scalar::<_, String>("SELECT name FROM locations WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Спавн-точка новых/пустых персонажей (`meta.spawn_location`, из сида).
pub async fn spawn_id(pool: &PgPool) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT value FROM meta WHERE key = 'spawn_location'")
        .fetch_optional(pool)
        .await
}

/// Персист позиции + бамп `state_version` одной транзакцией (§3, D27):
/// переход — критичное событие, флаш немедленный.
pub async fn move_char(
    pool: &PgPool,
    char_id: &str,
    location_id: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE characters SET location_id = $2, state_version = state_version + 1 \
         WHERE id = $1 RETURNING state_version",
    )
    .bind(char_id)
    .bind(location_id)
    .fetch_one(pool)
    .await
}

/// Пустой `location_id` персонажа → спавн (без бампа версии: не переход).
pub async fn assign_spawn(
    pool: &PgPool,
    char_id: &str,
    location_id: &str,
) -> Result<u64, sqlx::Error> {
    let result =
        sqlx::query("UPDATE characters SET location_id = $2 WHERE id = $1 AND location_id = ''")
            .bind(char_id)
            .bind(location_id)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

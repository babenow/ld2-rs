//! Контент-сид (D26, M12.3/M12.4): статический мир из `world.content_dir`
//! (JSON, версионируется в git) → PostgreSQL **идемпотентно**.
//!
//! Файлы каталога: `locations.json`, `skills.json`, `items.json` — все
//! обязательны. Сверка `meta.content_version` = sha256 по каноническому
//! дереву **всего** контента: версия не изменилась → БД не трогается
//! вообще (ручные правки переживают перезапуск — критерий M12.3);
//! изменилась → upsert локаций/навыков/предметов и обновление версии.
//! Удалённые из контента сущности из БД **не** дропаются (на них могут
//! ссылаться персонажи).

use std::path::{Path, PathBuf};

use ld2_protocol::types::{Entity, Exit, Requires};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// Строка content-файла: выход хранит целевую локацию (`to`) — на проводе
/// её быть не должно (§6 [`Exit`] без `to`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentExit {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub dir: Option<String>,
    /// Целевая локация (только БД/контент, в снапшот не попадает).
    pub to: String,
}

impl ContentExit {
    /// Проводной вид выхода (§6): без `to`.
    pub fn to_wire(&self) -> Exit {
        Exit {
            id: self.id.clone(),
            label: self.label.clone(),
            dir: self.dir.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentLocation {
    pub id: String,
    pub name: String,
    /// Единственная спавн-точка новых персонажей (→ `meta.spawn_location`).
    #[serde(default)]
    pub spawn: bool,
    #[serde(default)]
    pub flags: Vec<String>,
    pub text: String,
    #[serde(default)]
    pub exits: Vec<ContentExit>,
    #[serde(default)]
    pub entities: Vec<Entity>,
}

/// Навык из `skills.json` (skills.md §3: id + имя; группа — для UI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentSkill {
    pub id: String,
    pub name: String,
    pub group: String,
}

/// Определение предмета из `items.json` (§6 `ItemSnapshot`-ядро).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentItemDef {
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub name: String,
    pub rarity: String,
    #[serde(default)]
    pub upgrade: i32,
    #[serde(default)]
    pub stats: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub requires: Option<Requires>,
    #[serde(default)]
    pub gold_value: i64,
    /// Стартовое количество стака при взятии/выдаче.
    #[serde(default)]
    pub qty: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct LocationsRoot {
    locations: Vec<ContentLocation>,
}

#[derive(Debug, Deserialize)]
struct SkillsRoot {
    skills: Vec<ContentSkill>,
}

#[derive(Debug, Deserialize)]
struct ItemsRoot {
    items: Vec<ContentItemDef>,
}

/// Весь контент каталога (для версии и сида).
#[derive(Debug, Serialize)]
pub struct GameContent {
    locations: Vec<ContentLocation>,
    skills: Vec<ContentSkill>,
    items: Vec<ContentItemDef>,
}

#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("контент: не удалось прочитать {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("контент: некорректный JSON {0}: {1}")]
    Json(PathBuf, serde_json::Error),
    #[error("контент: {0}")]
    Db(#[from] sqlx::Error),
    #[error("контент: в {0} нет ни одной локации")]
    Empty(PathBuf),
}

/// Результат сида: что показать в логе старта.
#[derive(Debug)]
pub struct SeedReport {
    /// sha256-версия содержимого каталога.
    pub version: String,
    /// `false` — версия не изменилась, БД не трогалась.
    pub applied: bool,
}

/// Сид контента в PostgreSQL (D26). Вызывается при старте сервера.
pub async fn seed(pool: &PgPool, dir: impl AsRef<Path>) -> Result<SeedReport, ContentError> {
    let content = load(dir.as_ref())?;
    let version = content_version(&content)?;

    let current: Option<String> =
        sqlx::query_scalar("SELECT value FROM meta WHERE key = 'content_version'")
            .fetch_optional(pool)
            .await?;
    if current.as_deref() == Some(version.as_str()) {
        return Ok(SeedReport {
            version,
            applied: false,
        });
    }

    let spawn = content
        .locations
        .iter()
        .find(|l| l.spawn)
        .map(|l| l.id.clone());

    let mut tx = pool.begin().await?;
    for loc in &content.locations {
        let exits = serde_json::to_value(&loc.exits).unwrap_or_default();
        let entities = serde_json::to_value(&loc.entities).unwrap_or_default();
        sqlx::query(
            "INSERT INTO locations (id, name, flags, text, exits, entities) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (id) DO UPDATE SET \
               name = EXCLUDED.name, flags = EXCLUDED.flags, text = EXCLUDED.text, \
               exits = EXCLUDED.exits, entities = EXCLUDED.entities",
        )
        .bind(&loc.id)
        .bind(&loc.name)
        .bind(&loc.flags)
        .bind(&loc.text)
        .bind(exits)
        .bind(entities)
        .execute(&mut *tx)
        .await?;
    }
    for skill in &content.skills {
        sqlx::query(
            "INSERT INTO skills (id, name, skill_group) VALUES ($1, $2, $3) \
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, \
               skill_group = EXCLUDED.skill_group",
        )
        .bind(&skill.id)
        .bind(&skill.name)
        .bind(&skill.group)
        .execute(&mut *tx)
        .await?;
    }
    for item in &content.items {
        let stats = serde_json::Value::Object(item.stats.clone());
        let requires = serde_json::to_value(&item.requires).unwrap_or(serde_json::Value::Null);
        sqlx::query(
            "INSERT INTO item_defs (id, item_type, name, rarity, upgrade, stats, requires, gold_value, qty) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (id) DO UPDATE SET \
               item_type = EXCLUDED.item_type, name = EXCLUDED.name, rarity = EXCLUDED.rarity, \
               upgrade = EXCLUDED.upgrade, stats = EXCLUDED.stats, requires = EXCLUDED.requires, \
               gold_value = EXCLUDED.gold_value, qty = EXCLUDED.qty",
        )
        .bind(&item.id)
        .bind(&item.item_type)
        .bind(&item.name)
        .bind(&item.rarity)
        .bind(item.upgrade)
        .bind(stats)
        .bind(requires)
        .bind(item.gold_value)
        .bind(item.qty.unwrap_or(1))
        .execute(&mut *tx)
        .await?;
    }
    if let Some(spawn) = &spawn {
        sqlx::query(
            "INSERT INTO meta (key, value) VALUES ('spawn_location', $1) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
        )
        .bind(spawn)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "INSERT INTO meta (key, value) VALUES ('content_version', $1) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(&version)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    if spawn.is_none() {
        tracing::warn!("в контенте нет локации со spawn: true — новые персонажи без спавна");
    }
    Ok(SeedReport {
        version,
        applied: true,
    })
}

/// Загрузка трёх обязательных файлов каталога.
fn load(dir: &Path) -> Result<GameContent, ContentError> {
    let read = |name: &str| -> Result<String, ContentError> {
        let path = dir.join(name);
        std::fs::read_to_string(&path).map_err(|e| ContentError::Io(path, e))
    };

    let locations: LocationsRoot = serde_json::from_str(&read("locations.json")?)
        .map_err(|e| ContentError::Json(dir.join("locations.json"), e))?;
    let skills: SkillsRoot = serde_json::from_str(&read("skills.json")?)
        .map_err(|e| ContentError::Json(dir.join("skills.json"), e))?;
    let items: ItemsRoot = serde_json::from_str(&read("items.json")?)
        .map_err(|e| ContentError::Json(dir.join("items.json"), e))?;

    if locations.locations.is_empty() {
        return Err(ContentError::Empty(dir.to_path_buf()));
    }
    Ok(GameContent {
        locations: locations.locations,
        skills: skills.skills,
        items: items.items,
    })
}

/// Версия контента: sha256 от канонического дерева **всего** контента
/// (неизменное содержимое → неизменная версия независимо от имён файлов).
fn content_version(content: &GameContent) -> Result<String, ContentError> {
    let canonical = serde_json::to_string(content)
        .map_err(|e| ContentError::Json(PathBuf::from("<canonical>"), e))?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

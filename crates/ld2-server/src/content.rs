//! Контент-сид (D26, M12.3): статический мир из `world.content_dir`
//! (JSON, версионируется в git) → PostgreSQL **идемпотентно**.
//!
//! Сверка `meta.content_version` = sha256 по всем файлам каталога: версия
//! не изменилась → БД не трогается вообще (ручные правки в БД переживают
//! перезапуск — критерий M12.3); версия изменилась → upsert локаций и
//! обновление версии (контент — источник истины). Удалённые из контента
//! локации из БД **не** удаляются (на них могут ссылаться персонажи).

use std::path::{Path, PathBuf};

use ld2_protocol::types::{Entity, Exit};
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

/// Корень `locations.json`.
#[derive(Debug, Serialize, Deserialize)]
struct ContentRoot {
    locations: Vec<ContentLocation>,
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
    let dir = dir.as_ref();
    let root = load(dir)?;
    let version = content_version(&root)?;

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

    let spawn = root
        .locations
        .iter()
        .find(|l| l.spawn)
        .map(|l| l.id.clone());

    let mut tx = pool.begin().await?;
    for loc in &root.locations {
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

/// Загрузка всех `*.json` каталога (сортировка по имени — версия стабильна).
fn load(dir: &Path) -> Result<ContentRoot, ContentError> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| ContentError::Io(dir.to_path_buf(), e))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();

    let mut locations = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).map_err(|e| ContentError::Io(file.clone(), e))?;
        let root: ContentRoot =
            serde_json::from_str(&text).map_err(|e| ContentError::Json(file.clone(), e))?;
        locations.extend(root.locations);
    }
    if locations.is_empty() {
        return Err(ContentError::Empty(dir.to_path_buf()));
    }
    Ok(ContentRoot { locations })
}

/// Версия контента: sha256 от канонического дерева локаций (порядок полей —
/// порядок объявления, неизменное содержимое → неизменная версия независимо
/// от имён/числа файлов).
fn content_version(root: &ContentRoot) -> Result<String, ContentError> {
    let canonical = serde_json::to_string(&root.locations)
        .map_err(|e| ContentError::Json(PathBuf::from("<canonical>"), e))?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

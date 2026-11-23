//! Сид, догрузка и горячая перезагрузка контента (D26/M27).

use super::*;

/// Сид контента в PostgreSQL (D26). Вызывается при старте сервера и
/// подкомандой `seed` (M27): перечитывает каталог и применяет изменения.
pub async fn seed(pool: &PgPool, dir: impl AsRef<Path>) -> Result<SeedReport, ContentError> {
    let dir = dir.as_ref();
    let content = load(dir)?;
    validate(&content)?;
    // E6: версия — по всем 8 файлам (включая dialogs/shops), а не только
    // по типизированному сиду: правка диалогов/магазинов меняет версию.
    let raw = ld2_content::RawContent::load(dir)?;
    let version = content_version(&raw)?;
    seed_content(pool, &content, version).await
}

/// Загрузка контента через общую схему (`ld2-content`): типы/чтение —
/// одни для сервера и редактора (E1).
pub fn load(dir: &Path) -> Result<GameContent, ContentError> {
    ld2_content::load(dir).map_err(ContentError::Load)
}

/// Структурная валидация контента — общая с редактором (`ld2-content`).
pub fn validate(content: &GameContent) -> Result<(), ContentError> {
    match ld2_content::validate(content) {
        Ok(()) => Ok(()),
        // Фатальные проблемы остаются серверным variant'ом (тесты/сообщения).
        Err(ld2_content::ContentError::Invalid(problems)) => Err(ContentError::Invalid(problems)),
        Err(e) => Err(ContentError::Load(e)),
    }
}

/// Дешёвый отпечаток каталога (имя + размер + mtime файлов
/// [`CONTENT_FILES`]): poll сравнивает отпечаток и делает полную
/// загрузку/парсинг только при изменении. `None` — каталог/файлы не
/// читаются (watcher молчит и повторит на следующем тике).
pub fn fingerprint(dir: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for name in CONTENT_FILES {
        let meta = std::fs::metadata(dir.join(name)).ok()?;
        name.hash(&mut hasher);
        meta.len().hash(&mut hasher);
        let modified = meta.modified().ok()?;
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
            .hash(&mut hasher);
    }
    Some(hasher.finish())
}

/// Горячая перезагрузка (M27 фаза 1): перечитать каталог, провалидировать,
/// идемпотентно применить сид и вернуть части, которые сервер держит в
/// памяти. При ошибке (JSON/валидация/БД) текущее состояние сервера не
/// меняется — вызывающий решает, что делать (см. `reload::reload_state`).
pub async fn reload(pool: &PgPool, dir: impl AsRef<Path>) -> Result<ReloadReport, ContentError> {
    let dir = dir.as_ref();
    let content = load(dir)?;
    validate(&content)?;
    let raw = ld2_content::RawContent::load(dir)?;
    let version = content_version(&raw)?;
    let dialogs = crate::dialogs::load(dir)?;
    let shops = crate::shops::load(dir)?;
    let seeded = seed_content(pool, &content, version).await?;
    Ok(ReloadReport {
        version: seeded.version,
        applied: seeded.applied,
        locations: content.locations.len(),
        items: content.items.len(),
        dialogs,
        shops,
    })
}

/// sha256-версия контента (реализация — в `ld2-content`); E6: по сырым
/// 8 файлам, поэтому включает dialogs/shops.
fn content_version(raw: &ld2_content::RawContent) -> Result<String, ContentError> {
    ld2_content::version_raw(raw).map_err(ContentError::Load)
}

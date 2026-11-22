//! sha256-версия контента: каноническое дерево [`GameContent`].
//!
//! Неизменное содержимое → неизменная версия (не зависит от форматирования
//! файлов и порядка ключей в JSON: объекты канонизируются сортировкой
//! ключей). Версия пишется в `meta.content_version` (hot reload/сид) и
//! отдаётся в `/healthz`; редактор показывает её в статусе.
//!
//! Важно: сортировка нужна, потому что `serde_json` с `preserve_order`
//! хранит порядок ключей файла; переформатирование/перестановка ключей не
//! должны менять версию контента.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::model::GameContent;
use crate::raw::{ContentFile, RawContent};
use crate::ContentError;

/// Каноническая сериализация: объекты — с ключами по возрастанию.
fn canonical_json_value(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .into_iter()
                .map(|(k, v)| (k, canonical_json_value(v)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical_json_value).collect()),
        other => other,
    }
}

/// sha256-версия контента (каноническое дерево, 64 hex-символа).
pub fn version(content: &GameContent) -> Result<String, ContentError> {
    let value = serde_json::to_value(content)
        .map_err(|e| ContentError::Json(std::path::PathBuf::from("<canonical>"), e))?;
    hash_value(value)
}

/// sha256-версия **всех** редактируемых файлов (8): в отличие от
/// [`version`], включает `dialogs.json` и `shops.json` — их правка тоже
/// меняет версию контента (hot reload). Сервер и редактор считают одну и
/// ту же версию по сырым JSON-файлам (формат/порядок ключей не влияет).
pub fn version_raw(raw: &RawContent) -> Result<String, ContentError> {
    let mut files = serde_json::Map::new();
    for file in ContentFile::ALL {
        let key = file.name().trim_end_matches(".json").to_string();
        files.insert(key, raw.get(file).clone());
    }
    hash_value(serde_json::Value::Object(files))
}

fn hash_value(value: serde_json::Value) -> Result<String, ContentError> {
    let canonical = canonical_json_value(value);
    let text = serde_json::to_string(&canonical)
        .map_err(|e| ContentError::Json(std::path::PathBuf::from("<canonical>"), e))?;
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentFile, RawContent};
    use std::path::Path;

    /// Версия не зависит от порядка ключей в JSON-файлах (редактор/
    /// форматтер может переставить ключи — hot reload не должен
    /// перезаписывать БД без семантических изменений).
    #[test]
    fn version_ignores_object_key_order() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let raw = RawContent::load(&dir).expect("сырой контент");
        let before = version(&raw.typed().expect("typed")).expect("версия");

        // Переставляем ключи в stats первого предмета с непустым stats
        // (порядок обратный).
        let mut raw = raw;
        let index = raw.items["items"]
            .as_array()
            .expect("items[]")
            .iter()
            .position(|item| {
                item.get("stats")
                    .and_then(|stats| stats.as_object())
                    .map(|stats| !stats.is_empty())
                    .unwrap_or(false)
            })
            .expect("предмет со stats");
        let stats = raw.items["items"][index]["stats"]
            .as_object_mut()
            .expect("stats объекта");
        let mut pairs: Vec<(String, serde_json::Value)> =
            stats.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        pairs.reverse();
        stats.clear();
        for (key, value) in pairs {
            stats.insert(key, value);
        }

        let after = version(&raw.typed().expect("typed")).expect("версия");
        assert_eq!(before, after, "порядок ключей не влияет на версию");
    }

    /// Разные значения → разные версии.
    #[test]
    fn version_detects_content_changes() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let raw = RawContent::load(&dir).expect("сырой контент");
        let typed = raw.typed().expect("typed");
        let before = version(&typed).expect("версия");
        let mut changed = typed.clone();
        changed.items[0].gold_value += 1;
        assert_ne!(version(&changed).expect("версия"), before);
        assert_eq!(before.len(), 64);
        let _ = ContentFile::ALL; // файлы контента — единый каталог
    }

    /// E6: версия учитывает dialogs.json/shops.json (раньше — только 6
    /// файлов сида), и не зависит от порядка ключей в сыром JSON.
    #[test]
    fn version_raw_includes_dialogs_and_shops() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let raw = RawContent::load(&dir).expect("сырой контент");
        let before = version_raw(&raw).expect("версия");
        assert_eq!(before.len(), 64);

        let mut changed_dialog = raw.clone();
        if let Some(npc) = changed_dialog.dialogs["npcs"]
            .as_array_mut()
            .and_then(|npcs| npcs.first_mut())
        {
            npc["root"] = serde_json::Value::String("l_changed".into());
        }
        assert_ne!(version_raw(&changed_dialog).expect("версия"), before);

        let mut changed_shop = raw.clone();
        if let Some(npc) = changed_shop.shops["npcs"]
            .as_array_mut()
            .and_then(|npcs| npcs.first_mut())
        {
            npc["markup"] = serde_json::Value::from(2.5);
        }
        assert_ne!(version_raw(&changed_shop).expect("версия"), before);

        assert_eq!(before, version_raw(&raw).expect("детерминизм"));
    }
}

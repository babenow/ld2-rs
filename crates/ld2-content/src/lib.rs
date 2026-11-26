//! ld2-content — единая схема контента ld2 (M27/E1).
//!
//! Источник правды — файлы `content/*.json` в git. Крейт не зависит от БД/UI:
//! - сервер (`ld2-server`): загрузка/валидация/версия/сид/hot reload;
//! - редактор (`ld2-editor`): те же типы + `save` (E2), карта, формы.
//!
//! Неизвестные поля сохраняются (`extra`) — схема контента открытая (§6/§11).

pub mod load;
pub mod model;
pub mod raw;
pub mod validate;
pub mod version;

pub use load::{load, CONTENT_FILES};
pub use model::*;
pub use raw::{ContentFile, RawContent};
pub use validate::{
    validate, validate_items, validate_npc_content, validate_references, validate_report,
    ValidationReport, EFFECT_KINDS, ITEM_ATTRS, ITEM_CLASSES, ITEM_PARAMS, ITEM_RARITIES,
    ITEM_SLOTS, ITEM_STAT_KEYS, SPELL_SCHOOLS,
};
pub use version::{version, version_raw};

// Схема ссылается на типы контракта — редактору они тоже нужны.
pub use ld2_protocol::types::{Entity, EntityKind};

use std::path::PathBuf;

/// Ошибка чтения/валидации контента (без БД — сид в `ld2-server`).
#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("контент: не удалось прочитать {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("контент: некорректный JSON {0}: {1}")]
    Json(PathBuf, serde_json::Error),
    #[error("контент: в {0} нет ни одной локации")]
    Empty(PathBuf),
    /// Структурная валидация: дубликаты id и прочие фатальные ошибки.
    #[error("контент не прошёл валидацию: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn repo_content() -> GameContent {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        load(&dir).expect("контент репозитория загружается")
    }

    /// E1: репозиторный контент проходит общую валидацию.
    #[test]
    fn repository_content_is_valid() {
        assert!(validate(&repo_content()).is_ok());
    }

    /// E1 (открытая схема): неизвестные поля сохраняются при
    /// пересохранении — редактор не теряет данные сервера.
    #[test]
    fn unknown_fields_are_preserved() {
        let raw = r#"{
            "id": "l_probe", "name": "Проба", "text": "текст",
            "spawn": false, "flags": [], "exits": [], "entities": [],
            "custom_future_field": {"keep": [1, 2, 3]}
        }"#;
        let loc: ContentLocation = serde_json::from_str(raw).expect("десериализация");
        assert_eq!(
            loc.extra["custom_future_field"]["keep"][1],
            serde_json::json!(2)
        );
        let out = serde_json::to_value(&loc).expect("сериализация");
        assert_eq!(
            out["custom_future_field"]["keep"],
            serde_json::json!([1, 2, 3])
        );
    }

    /// E1: load → serialize → load не теряет данные (стабильный цикл).
    #[test]
    fn round_trip_preserves_content() {
        let content = repo_content();
        let json = serde_json::to_string(&content).expect("сериализация");
        let again: GameContent = serde_json::from_str(&json).expect("обратная десериализация");
        let json2 = serde_json::to_string(&again).expect("сериализация #2");
        assert_eq!(json, json2, "повторный цикл стабилен");
    }

    /// E1: версия — sha256 от канонического дерева; стабильна и
    /// чувствительна к изменениям контента.
    #[test]
    fn version_is_stable_and_content_sensitive() {
        let content = repo_content();
        let v1 = version(&content).expect("версия");
        let v2 = version(&content).expect("версия");
        assert_eq!(v1, v2, "версия детерминирована");
        assert_eq!(v1.len(), 64, "sha256 hex");

        let mut changed = content.clone();
        changed.locations[0].name.push_str(" (правка)");
        assert_ne!(version(&changed).expect("версия"), v1, "изменение видно");
    }
}

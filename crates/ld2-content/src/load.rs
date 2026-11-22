//! Загрузка каталога `content/` (6 обязательных файлов) в [`GameContent`].
//!
//! Ошибки — [`ContentError`] без зависимости от БД: сервер сам решает,
//! как сообщить о них (сид/hot reload), редактор показывает в UI.

use std::path::Path;

use serde::Deserialize;

use crate::model::*;
use crate::ContentError;

/// Файлы каталога, участвующие в hot reload/верстке контента: 6
/// обязательных файлов сида + диалоги/магазины (читаются сервером/редактором
/// отдельно, но входят в «каталог контента» как единое целое).
pub const CONTENT_FILES: [&str; 8] = [
    "locations.json",
    "skills.json",
    "items.json",
    "professions.json",
    "recipes.json",
    "quests.json",
    "dialogs.json",
    "shops.json",
];

#[derive(Debug, Deserialize)]
pub(crate) struct LocationsRoot {
    pub(crate) locations: Vec<ContentLocation>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SkillsRoot {
    pub(crate) skills: Vec<ContentSkill>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ItemsRoot {
    pub(crate) items: Vec<ContentItemDef>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ProfessionsRoot {
    pub(crate) professions: Vec<ContentProfession>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecipesRoot {
    pub(crate) recipes: Vec<ContentRecipe>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct QuestsRoot {
    pub(crate) quests: Vec<ContentQuest>,
}

/// Загрузка обязательных файлов каталога (M27: `pub` — hot reload и
/// подкоманда `seed` переиспользуют её).
pub fn load(dir: &Path) -> Result<GameContent, ContentError> {
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
    let professions: ProfessionsRoot = serde_json::from_str(&read("professions.json")?)
        .map_err(|e| ContentError::Json(dir.join("professions.json"), e))?;
    let recipes: RecipesRoot = serde_json::from_str(&read("recipes.json")?)
        .map_err(|e| ContentError::Json(dir.join("recipes.json"), e))?;
    let quests: QuestsRoot = serde_json::from_str(&read("quests.json")?)
        .map_err(|e| ContentError::Json(dir.join("quests.json"), e))?;

    if locations.locations.is_empty() {
        return Err(ContentError::Empty(dir.to_path_buf()));
    }
    Ok(GameContent {
        locations: locations.locations,
        skills: skills.skills,
        items: items.items,
        professions: professions.professions,
        recipes: recipes.recipes,
        quests: quests.quests,
    })
}

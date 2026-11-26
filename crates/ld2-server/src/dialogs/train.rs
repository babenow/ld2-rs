//! Обучение у NPC и загрузка конфигурации диалогов.

use super::*;

pub(super) fn training_qty() -> i64 {
    1
}

pub(super) fn default_qty() -> i64 {
    1
}

/// Загрузка `dialogs.json`: файла нет → пусто (диалоги опциональны),
/// сломанный JSON → ошибка (в `AppState::new` — warn + пусто; валидность
/// контента ловят контент-тесты в CI).
pub fn load(dir: &Path) -> Result<HashMap<String, DialogNpc>, ContentError> {
    let path = dir.join("dialogs.json");
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| ContentError::Io(path.clone(), e))?;
    let root: DialogsRoot = serde_json::from_str(&raw).map_err(|e| ContentError::Json(path, e))?;
    Ok(root
        .npcs
        .into_iter()
        .map(|npc| (npc.id.clone(), npc))
        .collect())
}

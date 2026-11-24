//! Отчёт валидации и полная проверка контента.

use super::*;

/// Структурная валидация контента перед сидом/hot reload (M27 фаза 1).
///
/// **Фатально** (сид/hot reload отказываются применять, старое состояние
/// сохраняется): дубликаты id — upsert молча перетёр бы записи в БД.
/// **Предупреждение** (warn + применяем): висячие выходы — карта мира
/// отфильтровывает их, переход вернёт `E_TARGET_GONE`; во время мержа
/// карт это допустимое промежуточное состояние, валить старт сервера ради
/// него нельзя.
/// Отчёт валидации: фатальные ошибки и предупреждения (редактор
/// показывает их списком, сервер — блокирует сид при `errors`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ValidationReport {
    /// Контент можно применять (нет фатальных ошибок).
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Полный отчёт валидации (для редактора/диагностики); сервер использует
/// [`validate`] — отказ при ошибках.
pub fn validate_report(content: &GameContent) -> ValidationReport {
    use std::collections::HashSet;

    let mut problems: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut loc_ids: HashSet<&str> = HashSet::with_capacity(content.locations.len());
    for loc in &content.locations {
        if !loc_ids.insert(loc.id.as_str()) {
            problems.push(format!("локация: дубликат id {}", loc.id));
        }
    }
    for loc in &content.locations {
        let mut entity_ids: HashSet<&str> = HashSet::with_capacity(loc.entities.len());
        for entity in &loc.entities {
            if !entity_ids.insert(entity.id.as_str()) {
                problems.push(format!(
                    "локация {}: дубликат сущности {}",
                    loc.id, entity.id
                ));
            }
        }
        let mut exit_ids: HashSet<&str> = HashSet::with_capacity(loc.exits.len());
        for exit in &loc.exits {
            if !exit_ids.insert(exit.id.as_str()) {
                problems.push(format!(
                    "локация {}: дубликат id выхода {}",
                    loc.id, exit.id
                ));
            }
            if !loc_ids.contains(exit.to.as_str()) {
                warnings.push(format!(
                    "локация {}: выход {} ведёт в несуществующую {}",
                    loc.id, exit.id, exit.to
                ));
            }
        }
    }

    let mut seen: HashSet<&str> = HashSet::new();
    for skill in &content.skills {
        if !seen.insert(skill.id.as_str()) {
            problems.push(format!("навык: дубликат id {}", skill.id));
        }
    }
    seen.clear();
    for item in &content.items {
        if !seen.insert(item.id.as_str()) {
            problems.push(format!("предмет: дубликат id {}", item.id));
        }
    }
    seen.clear();
    for prof in &content.professions {
        if !seen.insert(prof.id.as_str()) {
            problems.push(format!("профессия: дубликат id {}", prof.id));
        }
    }
    seen.clear();
    for recipe in &content.recipes {
        if !seen.insert(recipe.id.as_str()) {
            problems.push(format!("рецепт: дубликат id {}", recipe.id));
        }
    }
    seen.clear();
    for quest in &content.quests {
        if !seen.insert(quest.id.as_str()) {
            problems.push(format!("квест: дубликат id {}", quest.id));
        }
    }

    ValidationReport {
        errors: problems,
        warnings,
    }
}

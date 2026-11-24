//! Ссылочная целостность контента (висячие выходы).

use super::*;

/// Ссылочная валидация (E5, только предупреждения): рецепты и квесты
/// ссылаются на существующие предметы/навыки/профессии/NPC/станции.
///
/// Не блокирует сид/сохранение: в контенте бывают временные висячие
/// ссылки (WIP), но редактор показывает их контент-мейкеру списком.
/// Предметы в ингредиентах/награде адресуются и по `id`, и по `type`
/// (исторически оба варианта, `herb` — и id, и type), поэтому проверка
/// принимает оба пространства имён.
pub fn validate_references(content: &GameContent) -> Vec<String> {
    use std::collections::HashSet;

    let mut warnings = Vec::new();
    let item_ids: HashSet<&str> = content.items.iter().map(|item| item.id.as_str()).collect();
    let item_types: HashSet<&str> = content
        .items
        .iter()
        .map(|item| item.item_type.as_str())
        .collect();
    let skill_ids: HashSet<&str> = content
        .skills
        .iter()
        .map(|skill| skill.id.as_str())
        .collect();
    let profession_ids: HashSet<&str> = content
        .professions
        .iter()
        .map(|profession| profession.id.as_str())
        .collect();
    let location_ids: HashSet<&str> = content
        .locations
        .iter()
        .map(|location| location.id.as_str())
        .collect();
    let npc_ids: HashSet<&str> = content
        .locations
        .iter()
        .flat_map(|location| location.entities.iter())
        .filter(|entity| entity.kind == EntityKind::Npc)
        .map(|entity| entity.id.as_str())
        .collect();
    let monster_ids: HashSet<&str> = content
        .locations
        .iter()
        .flat_map(|location| location.entities.iter())
        .filter(|entity| entity.kind == EntityKind::Monster)
        .map(|entity| entity.id.as_str())
        .collect();
    let station_ids: HashSet<&str> = content
        .locations
        .iter()
        .flat_map(|location| location.entities.iter())
        .filter_map(|entity| entity.extra.get("station"))
        .filter_map(|station| station.as_str())
        .collect();

    // Предмет в ингредиентах/награде: id или type (смешанная адресация).
    let known_item = |value: &str| item_ids.contains(value) || item_types.contains(value);

    for recipe in &content.recipes {
        if !profession_ids.contains(recipe.profession.as_str()) {
            warnings.push(format!(
                "рецепт {}: профессия {} не найдена",
                recipe.id, recipe.profession
            ));
        }
        if let Some(station) = &recipe.station {
            if !station_ids.contains(station.as_str()) {
                warnings.push(format!(
                    "рецепт {}: станок {} не найден ни у одного объекта",
                    recipe.id, station
                ));
            }
        }
        if let Some(tool) = &recipe.tool {
            if !item_types.contains(tool.item_type.as_str()) {
                warnings.push(format!(
                    "рецепт {}: инструмент типа {} — предметов такого типа нет",
                    recipe.id, tool.item_type
                ));
            }
        }
        for (index, input) in recipe.inputs.iter().enumerate() {
            if !known_item(input.item_type.as_str()) {
                warnings.push(format!(
                    "рецепт {}: ингредиент #{index} — неизвестный предмет {}",
                    recipe.id, input.item_type
                ));
            }
        }
        if !known_item(recipe.output.item_type.as_str()) {
            warnings.push(format!(
                "рецепт {}: результат — неизвестный предмет {}",
                recipe.id, recipe.output.item_type
            ));
        }
        for skill in recipe.requires.skill.keys() {
            if !skill_ids.contains(skill.as_str()) {
                warnings.push(format!(
                    "рецепт {}: требование к навыку {} не найдено",
                    recipe.id, skill
                ));
            }
        }
    }

    for quest in &content.quests {
        if let Some(giver) = &quest.giver {
            if !npc_ids.contains(giver.as_str()) && !location_ids.contains(giver.as_str()) {
                warnings.push(format!(
                    "квест {}: выдающий NPC {} не найден",
                    quest.id, giver
                ));
            }
        }
        for (index, goal) in quest.goals.iter().enumerate() {
            if goal.goal_type == "kill" {
                match &goal.target {
                    Some(target) if !monster_ids.contains(target.as_str()) => {
                        warnings.push(format!(
                            "квест {}: цель #{index} — монстр {} не найден",
                            quest.id, target
                        ));
                    }
                    None => warnings.push(format!(
                        "квест {}: цель #{index} (kill) без target",
                        quest.id
                    )),
                    _ => {}
                }
            }
        }
        for (index, item) in quest.reward.items.iter().enumerate() {
            if !known_item(item.item_type.as_str()) {
                warnings.push(format!(
                    "квест {}: награда #{index} — неизвестный предмет {}",
                    quest.id, item.item_type
                ));
            }
        }
        if let Some(rank_up) = &quest.reward.rank_up {
            if !profession_ids.contains(rank_up.as_str()) {
                warnings.push(format!(
                    "квест {}: повышение ранга {} — профессия не найдена",
                    quest.id, rank_up
                ));
            }
        }
    }

    warnings
}

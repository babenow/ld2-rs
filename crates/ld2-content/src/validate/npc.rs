//! Валидация диалогов NPC, магазинов и ссылок между контентом (RF2i).

use std::collections::HashSet;

use crate::model::{ContentDialogNpc, ContentDialogs, ContentShops, GameContent};
use ld2_protocol::types::EntityKind;

use super::report::ValidationReport;

/// Наборы id контента для проверок ссылок диалогов.
struct DialogRefs<'a> {
    item_ids: &'a HashSet<&'a str>,
    skill_ids: &'a HashSet<&'a str>,
    profession_ids: &'a HashSet<&'a str>,
    quest_ids: &'a HashSet<&'a str>,
    shop_npc_ids: &'a HashSet<&'a str>,
}

/// Валидация диалогов NPC, магазинов, обучения и ссылок внутри них.
pub fn validate_npc_content(
    dialogs: &ContentDialogs,
    shops: &ContentShops,
    content: &GameContent,
) -> ValidationReport {
    use std::collections::{HashMap, HashSet};
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let item_ids: HashSet<&str> = content.items.iter().map(|item| item.id.as_str()).collect();
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
    let quest_ids: HashSet<&str> = content
        .quests
        .iter()
        .map(|quest| quest.id.as_str())
        .collect();
    let npc_entities: HashMap<&str, &ld2_protocol::types::Entity> = content
        .locations
        .iter()
        .flat_map(|location| location.entities.iter())
        .filter(|entity| entity.kind == EntityKind::Npc)
        .map(|entity| (entity.id.as_str(), entity))
        .collect();
    let dialog_npc_ids: HashSet<&str> = dialogs.npcs.iter().map(|npc| npc.id.as_str()).collect();
    let shop_npc_ids: HashSet<&str> = shops.npcs.iter().map(|npc| npc.id.as_str()).collect();
    check_duplicate_ids(dialogs, shops, &mut errors);
    let refs = DialogRefs {
        item_ids: &item_ids,
        skill_ids: &skill_ids,
        profession_ids: &profession_ids,
        quest_ids: &quest_ids,
        shop_npc_ids: &shop_npc_ids,
    };
    for npc in &dialogs.npcs {
        check_npc_dialog(npc, &npc_entities, &refs, &mut errors, &mut warnings);
    }
    check_quest_givers(content, &npc_entities, &dialog_npc_ids, &mut warnings);
    check_shop_content(shops, &npc_entities, &item_ids, &mut errors, &mut warnings);
    ValidationReport { errors, warnings }
}

fn check_duplicate_ids(dialogs: &ContentDialogs, shops: &ContentShops, errors: &mut Vec<String>) {
    let mut seen_npcs: HashSet<&str> = HashSet::new();
    for npc in &dialogs.npcs {
        if !seen_npcs.insert(npc.id.as_str()) {
            errors.push(format!("диалоги: дубликат NPC {}", npc.id));
        }
    }
    let mut seen_shops: HashSet<&str> = HashSet::new();
    for shop in &shops.npcs {
        if !seen_shops.insert(shop.id.as_str()) {
            errors.push(format!("магазины: дубликат NPC {}", shop.id));
        }
    }
}

fn check_npc_dialog(
    npc: &ContentDialogNpc,
    npc_entities: &std::collections::HashMap<&str, &ld2_protocol::types::Entity>,
    refs: &DialogRefs<'_>,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    if !npc_entities.contains_key(npc.id.as_str()) {
        warnings.push(format!(
            "диалоги: NPC {} не найден среди сущностей локаций",
            npc.id
        ));
    }
    if npc.root.is_none() {
        warnings.push(format!("диалоги: у {} нет root (стартовой строки)", npc.id));
    }
    let line_ids: HashSet<&str> = npc.lines.iter().map(|line| line.id.as_str()).collect();
    if let Some(root) = &npc.root {
        if npc.lines.iter().all(|line| line.id != *root) {
            warnings.push(format!(
                "диалоги: {} — корень {} отсутствует в строках",
                npc.id, root
            ));
        }
    }
    let mut seen_lines: HashSet<&str> = HashSet::new();
    let mut seen_replies: HashSet<&str> = HashSet::new();
    for line in &npc.lines {
        if !seen_lines.insert(line.id.as_str()) {
            errors.push(format!(
                "диалоги: {} — дубликат id строки {}",
                npc.id, line.id
            ));
        }
        if let Some(every) = line.every_s {
            if every <= 0 {
                errors.push(format!(
                    "диалоги: {} — every_s у строки {} должен быть > 0",
                    npc.id, line.id
                ));
            }
        }
        if let Some(quest) = &line.quest {
            if !refs.quest_ids.contains(quest.as_str()) {
                warnings.push(format!(
                    "диалоги: {} — строка {} ссылается на неизвестный квест {}",
                    npc.id, line.id, quest
                ));
            }
        }
        if let Some(when) = &line.when {
            validate_dialog_when(
                warnings,
                &format!("диалоги: {} — строка {}", npc.id, line.id),
                when,
                refs.item_ids,
                refs.skill_ids,
                refs.profession_ids,
            );
        }
        for reply in &line.replies {
            if !seen_replies.insert(reply.id.as_str()) {
                errors.push(format!(
                    "диалоги: {} — дубликат id реплики {}",
                    npc.id, reply.id
                ));
            }
            if let Some(next) = &reply.next {
                if !line_ids.contains(next.as_str()) {
                    warnings.push(format!(
                        "диалоги: {} — реплика {} ведёт на несуществующую строку {}",
                        npc.id, reply.id, next
                    ));
                }
            }
            if let Some(when) = &reply.when {
                validate_dialog_when(
                    warnings,
                    &format!("диалоги: {} — реплика {}", npc.id, reply.id),
                    when,
                    refs.item_ids,
                    refs.skill_ids,
                    refs.profession_ids,
                );
            }
            match reply.action.as_deref() {
                None => {}
                Some("shop") => {
                    if !refs.shop_npc_ids.contains(npc.id.as_str()) {
                        warnings.push(format!(
                            "диалоги: {} — action=shop у реплики {}, но магазина нет",
                            npc.id, reply.id
                        ));
                    }
                }
                Some("train") => {
                    if npc.training.is_empty() {
                        warnings.push(format!(
                            "диалоги: {} — action=train у реплики {}, но training пуст",
                            npc.id, reply.id
                        ));
                    }
                }
                Some(other) => errors.push(format!(
                    "диалоги: {} — реплика {}: неизвестное действие {other}",
                    npc.id, reply.id
                )),
            }
        }
    }

    let mut trained: HashSet<&str> = HashSet::new();
    for entry in &npc.training {
        if !refs.skill_ids.contains(entry.skill.as_str()) {
            warnings.push(format!(
                "диалоги: {} — training: неизвестный навык {}",
                npc.id, entry.skill
            ));
        }
        if !trained.insert(entry.skill.as_str()) {
            warnings.push(format!(
                "диалоги: {} — training: навык {} указан дважды",
                npc.id, entry.skill
            ));
        }
        if entry.points <= 0 {
            warnings.push(format!(
                "диалоги: {} — training.{}: points должен быть > 0",
                npc.id, entry.skill
            ));
        }
        if entry.gold < 0 {
            warnings.push(format!(
                "диалоги: {} — training.{}: gold должен быть ≥ 0",
                npc.id, entry.skill
            ));
        }
        for (index, item) in entry.items.iter().enumerate() {
            if !refs.item_ids.contains(item.def_id.as_str()) {
                warnings.push(format!(
                    "диалоги: {} — training.{}: плата #{index} — неизвестный предмет {}",
                    npc.id, entry.skill, item.def_id
                ));
            }
            if item.qty <= 0 {
                warnings.push(format!(
                    "диалоги: {} — training.{}: плата #{index} — qty должен быть > 0",
                    npc.id, entry.skill
                ));
            }
        }
        let marker_trains = npc_entities
            .get(npc.id.as_str())
            .and_then(|entity| entity.extra.get("trainer"))
            .and_then(|trainer| trainer.get("skills"))
            .and_then(|skills| skills.as_array())
            .is_some_and(|skills| {
                skills
                    .iter()
                    .any(|value| value.as_str() == Some(entry.skill.as_str()))
            });
        if !marker_trains {
            warnings.push(format!(
                "диалоги: {} — training.{}, но маркера trainer.skills в locations.json нет",
                npc.id, entry.skill
            ));
        }
    }
}

fn check_quest_givers(
    content: &GameContent,
    npc_entities: &std::collections::HashMap<&str, &ld2_protocol::types::Entity>,
    dialog_npc_ids: &std::collections::HashSet<&str>,
    warnings: &mut Vec<String>,
) {
    for quest in &content.quests {
        if let Some(giver) = &quest.giver {
            if npc_entities.contains_key(giver.as_str()) && !dialog_npc_ids.contains(giver.as_str())
            {
                warnings.push(format!(
                    "квест {}: выдающий {} есть в локациях, но записи диалога нет",
                    quest.id, giver
                ));
            }
        }
    }
}

fn check_shop_content(
    shops: &ContentShops,
    npc_entities: &std::collections::HashMap<&str, &ld2_protocol::types::Entity>,
    item_ids: &std::collections::HashSet<&str>,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    for shop in &shops.npcs {
        if !npc_entities.contains_key(shop.id.as_str()) {
            warnings.push(format!(
                "магазины: NPC {} не найден среди сущностей локаций",
                shop.id
            ));
        }
        if shop.markup <= 0.0 {
            errors.push(format!("магазины: {} — markup должен быть > 0", shop.id));
        }
        if shop.currencies.is_empty() {
            warnings.push(format!("магазины: {} — нет валютных вкладок", shop.id));
        }
        let mut seen_currencies: HashSet<&str> = HashSet::new();
        for tab in &shop.currencies {
            if !seen_currencies.insert(tab.currency.as_str()) {
                errors.push(format!(
                    "магазины: {} — дубликат валюты {}",
                    shop.id, tab.currency
                ));
            }
            if !item_ids.contains(tab.currency.as_str()) {
                warnings.push(format!(
                    "магазины: {} — валюта {} не найдена среди предметов",
                    shop.id, tab.currency
                ));
            }
            if tab.rate <= 0.0 {
                errors.push(format!(
                    "магазины: {} — rate валюты {} должен быть > 0",
                    shop.id, tab.currency
                ));
            }
            if tab.items.is_empty() {
                warnings.push(format!(
                    "магазины: {} — пустая вкладка {}",
                    shop.id, tab.currency
                ));
            }
            for item in &tab.items {
                if !item_ids.contains(item.as_str()) {
                    warnings.push(format!(
                        "магазины: {} — товар {} не найден среди предметов",
                        shop.id, item
                    ));
                }
            }
        }
    }
}

fn validate_dialog_when(
    warnings: &mut Vec<String>,
    context: &str,
    when: &crate::model::ContentDialogWhen,
    item_ids: &std::collections::HashSet<&str>,
    skill_ids: &std::collections::HashSet<&str>,
    profession_ids: &std::collections::HashSet<&str>,
) {
    if let Some(item) = &when.item {
        if !item_ids.contains(item.def_id.as_str()) {
            warnings.push(format!(
                "{context}: условие item — неизвестный предмет {}",
                item.def_id
            ));
        }
    }
    if let Some(skill) = &when.skill {
        if !skill_ids.contains(skill.id.as_str()) {
            warnings.push(format!(
                "{context}: условие skill — неизвестный навык {}",
                skill.id
            ));
        }
    }
    if let Some(profession) = &when.profession {
        if !profession_ids.contains(profession.id.as_str()) {
            warnings.push(format!(
                "{context}: условие profession — неизвестная профессия {}",
                profession.id
            ));
        }
    }
    if let Some(gender) = &when.gender {
        if !matches!(gender.as_str(), "male" | "female") {
            warnings.push(format!(
                "{context}: условие gender — неизвестный пол {gender}"
            ));
        }
    }
}

//! Структурная валидация контента (дубликаты id, висячие выходы).
//!
//! Общая для сервера (сид/hot reload) и редактора: дубликаты id —
//! фатальны (upsert молча перетёр бы записи в БД), висячие выходы —
//! предупреждение (мерж карт может оставлять их временно).

use crate::model::{ContentDialogs, ContentShops, GameContent};
use crate::ContentError;
use ld2_protocol::types::EntityKind;

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

/// Валидация диалогов и магазинов (E5.2): структурные ошибки —
/// дубликаты id строк/реплик/валют и недопустимые значения; висячие
/// ссылки (неизвестные NPC/навыки/предметы/строки `next`) и несогласия
/// с `locations.json` — предупреждения (WIP-контент не блокируем).
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

    for npc in &dialogs.npcs {
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
                if !quest_ids.contains(quest.as_str()) {
                    warnings.push(format!(
                        "диалоги: {} — строка {} ссылается на неизвестный квест {}",
                        npc.id, line.id, quest
                    ));
                }
            }
            if let Some(when) = &line.when {
                validate_dialog_when(
                    &mut warnings,
                    &format!("диалоги: {} — строка {}", npc.id, line.id),
                    when,
                    &item_ids,
                    &skill_ids,
                    &profession_ids,
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
                        &mut warnings,
                        &format!("диалоги: {} — реплика {}", npc.id, reply.id),
                        when,
                        &item_ids,
                        &skill_ids,
                        &profession_ids,
                    );
                }
                match reply.action.as_deref() {
                    None => {}
                    Some("shop") => {
                        if !shop_npc_ids.contains(npc.id.as_str()) {
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
            if !skill_ids.contains(entry.skill.as_str()) {
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
                if !item_ids.contains(item.def_id.as_str()) {
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

    ValidationReport { errors, warnings }
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

/// Валидация для сервера: `Err(Invalid)` при фатальных проблемах,
/// предупреждения — в лог.
pub fn validate(content: &GameContent) -> Result<(), ContentError> {
    let report = validate_report(content);
    for warning in &report.warnings {
        tracing::warn!("контент (не критично): {warning}");
    }
    if report.is_valid() {
        Ok(())
    } else {
        Err(ContentError::Invalid(report.errors.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentDialogLine, ContentDialogReply};
    use std::path::Path;

    fn repo_content() -> GameContent {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        crate::load(&dir).expect("контент репозитория загружается")
    }

    /// E5: контент репозитория не имеет висячих ссылок.
    #[test]
    fn repository_references_are_clean() {
        let warnings = validate_references(&repo_content());
        assert!(warnings.is_empty(), "висячие ссылки: {warnings:?}");
    }

    /// E5: битая ссылка в квесте — предупреждение (не ошибка).
    #[test]
    fn broken_quest_references_are_warnings() {
        let mut content = repo_content();
        content.quests[0].giver = Some("npc_no_such".into());
        content.quests[0].goals[0].target = Some("m_no_such".into());
        content.quests[0].reward.rank_up = Some("no_profession".into());
        let warnings = validate_references(&content);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("npc_no_such")));
        assert!(warnings.iter().any(|w| w.contains("m_no_such")));
        assert!(warnings.iter().any(|w| w.contains("no_profession")));
    }

    /// E5: ингредиент по `type` (flask) и по `id` (herb) — оба валидны;
    /// неизвестный предмет — предупреждение.
    #[test]
    fn recipe_inputs_accept_ids_and_types() {
        let mut content = repo_content();
        let index = content
            .recipes
            .iter()
            .position(|recipe| recipe.id == "rec_potion_heal")
            .expect("рецепт");
        content.recipes[index].inputs[0].item_type = "herb".into();
        content.recipes[index].output.item_type = "flask".into();
        assert!(
            validate_references(&content).is_empty(),
            "id и type валидны"
        );

        content.recipes[index].inputs[0].item_type = "no_such_item".into();
        let warnings = validate_references(&content);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("no_such_item"));
    }

    fn repo_npc() -> (ContentDialogs, ContentShops, GameContent) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../content");
        let raw = crate::RawContent::load(&dir).expect("сырой контент");
        let dialogs = raw.typed_dialogs().expect("dialogs.json");
        let shops = raw.typed_shops().expect("shops.json");
        (dialogs, shops, repo_content())
    }

    /// E5.2: диалоги и магазины репозитория проходят ссылочную валидацию.
    #[test]
    fn repository_npc_content_is_clean() {
        let (dialogs, shops, content) = repo_npc();
        let report = validate_npc_content(&dialogs, &shops, &content);
        assert!(report.errors.is_empty(), "ошибки: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "предупреждения: {:?}",
            report.warnings
        );
    }

    /// E5.2: битые ссылки диалогов/магазинов — предупреждения (не ошибки).
    #[test]
    fn broken_npc_references_are_warnings() {
        let (mut dialogs, mut shops, content) = repo_npc();
        dialogs.npcs[0].root = Some("l_missing".into());
        dialogs.npcs[0].lines[0].quest = Some("q_missing".into());
        if let Some(reply) = dialogs.npcs[0].lines[0].replies.first_mut() {
            reply.next = Some("l_missing".into());
        }
        shops.npcs[0].currencies[0]
            .items
            .push("no_such_item".into());
        let report = validate_npc_content(&dialogs, &shops, &content);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let warnings = report.warnings.join("; ");
        for needle in ["l_missing", "q_missing", "no_such_item"] {
            assert!(warnings.contains(needle), "нет {needle}: {warnings}");
        }
    }

    /// E5.2: дубликаты строк и реплик в диалоге — фатальные ошибки.
    #[test]
    fn duplicate_dialog_ids_are_errors() {
        let (mut dialogs, shops, content) = repo_npc();
        let line_id = dialogs.npcs[0].lines[0].id.clone();
        let line_text = dialogs.npcs[0].lines[0].text.clone();
        dialogs.npcs[0].lines.push(ContentDialogLine {
            id: line_id,
            text: line_text,
            ..Default::default()
        });
        let reply_id = dialogs.npcs[0].lines[0]
            .replies
            .first()
            .map(|reply| reply.id.clone())
            .expect("у строки есть реплики");
        dialogs.npcs[0].lines[0].replies.push(ContentDialogReply {
            id: reply_id,
            ..Default::default()
        });
        let report = validate_npc_content(&dialogs, &shops, &content);
        assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
        assert!(report.errors.iter().all(|error| error.contains("дубликат")));
    }
}

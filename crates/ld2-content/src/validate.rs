//! Структурная валидация контента (дубликаты id, висячие выходы).
//!
//! Общая для сервера (сид/hot reload) и редактора: дубликаты id —
//! фатальны (upsert молча перетёр бы записи в БД), висячие выходы —
//! предупреждение (мерж карт может оставлять их временно).

use crate::model::{ContentDialogs, ContentShops, GameContent};
use crate::ContentError;
use ld2_protocol::types::EntityKind;

// ─── Схема предметов (§1.1 docs/data-model.md) ─────────────────────────────

/// Редкости предмета (`items.json`: `rarity`).
pub const ITEM_RARITIES: [&str; 5] = ["common", "uncommon", "rare", "epic", "legendary"];
/// Атрибуты персонажа (ADR-010): `bonuses[].id` при `kind = "attr"` и
/// ключ `stats.attr`.
pub const ITEM_ATTRS: [&str; 4] = ["str", "dex", "vit", "int"];
/// Боевые параметры (ADR-010): `bonuses[].id` при `kind = "param"` и
/// ключи `stats.param`.
pub const ITEM_PARAMS: [&str; 20] = [
    "accuracy",
    "damage",
    "damage_min",
    "damage_max",
    "crit_chance",
    "crit_damage",
    "armor",
    "dodge",
    "parry_min",
    "parry_max",
    "parry_chance",
    "block_min",
    "block_max",
    "block_chance",
    "magic_defense",
    "magic_dodge",
    "magic_resist",
    "magic_dispel",
    "magic_crit_damage",
    "magic_crit_chance",
];
/// Слоты экипировки (`items.json`: `slot`).
pub const ITEM_SLOTS: [&str; 13] = [
    "main_hand",
    "off_hand",
    "head",
    "body",
    "legs",
    "feet",
    "gloves",
    "shoulders",
    "bracers",
    "cloak",
    "ring",
    "amulet",
    "artifact",
];
/// Классы персонажей для `stats.class` рун.
pub const ITEM_CLASSES: [&str; 3] = ["warrior", "archer", "mage"];
/// Школы магии для `stats.school` заклинаний и рун.
pub const SPELL_SCHOOLS: [&str; 3] = ["fire", "frost", "storm"];
/// Эффекты `stats.effect.kind` (контроль/DoT).
pub const EFFECT_KINDS: [&str; 4] = ["stun", "root", "bleed", "burn"];
/// Известные ключи `stats` — редактор показывает их формами; незнакомые
/// ключи сохраняются как есть (открытая схема) и дают предупреждение.
pub const ITEM_STAT_KEYS: [&str; 20] = [
    "dmg", "param", "armor", "magic", "attr", "value", "heal", "spell", "damage", "school",
    "charges", "max", "glyph", "class", "cost", "chance", "quality", "buff", "buff2", "effect",
];

/// Значение `stats`-поля как число (с учётом `{value: N}`-обёртки).
fn num(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::Object(map) => map.get("value").and_then(num),
        _ => None,
    }
}

/// Неизвестный числовой параметр в `stats.param`: {crit_chance: 3}.
fn validate_item_params(
    stats: &serde_json::Map<String, serde_json::Value>,
    item_id: &str,
    warnings: &mut Vec<String>,
) {
    let Some(params) = stats.get("param") else {
        return;
    };
    let serde_json::Value::Object(params) = params else {
        warnings.push(format!(
            "предмет {item_id}: stats.param должен быть объектом"
        ));
        return;
    };
    for (param, value) in params {
        if !ITEM_PARAMS.contains(&param.as_str()) {
            warnings.push(format!(
                "предмет {item_id}: stats.param — неизвестный параметр {param}"
            ));
        }
        if num(value).is_none() {
            warnings.push(format!(
                "предмет {item_id}: stats.param.{param} — значение должно быть числом"
            ));
            continue;
        }
        if num(value).is_some_and(|value| value == 0.0) {
            warnings.push(format!(
                "предмет {item_id}: stats.param.{param} — значение не может быть 0"
            ));
        }
        if let serde_json::Value::Object(map) = value {
            if let Some(pct) = map.get("pct") {
                if !pct.is_boolean() {
                    warnings.push(format!(
                        "предмет {item_id}: stats.param.{param}.pct должен быть bool"
                    ));
                }
            }
        }
    }
}

/// Автобафф (`stats.buff` / `stats.buff2`): параметр, значение, длительность,
/// стаки и процентный флаг (ADR-010).
fn validate_item_buff(
    stats: &serde_json::Map<String, serde_json::Value>,
    slot: &str,
    item_id: &str,
    warnings: &mut Vec<String>,
) {
    let Some(buff) = stats.get(slot) else {
        return;
    };
    let serde_json::Value::Object(buff) = buff else {
        warnings.push(format!(
            "предмет {item_id}: stats.{slot} должен быть объектом"
        ));
        return;
    };
    match buff.get("param").and_then(|value| value.as_str()) {
        Some(param) if ITEM_PARAMS.contains(&param) => {}
        Some(param) => warnings.push(format!(
            "предмет {item_id}: stats.{slot} — неизвестный параметр {param}"
        )),
        None => warnings.push(format!("предмет {item_id}: stats.{slot} без param")),
    }
    match buff.get("value").and_then(num) {
        Some(value) if value != 0.0 => {}
        Some(_) => warnings.push(format!(
            "предмет {item_id}: stats.{slot}.value не может быть 0"
        )),
        None => warnings.push(format!(
            "предмет {item_id}: stats.{slot} без значения value"
        )),
    }
    match buff.get("duration_s").and_then(num) {
        Some(duration) if duration > 0.0 => {}
        Some(_) => warnings.push(format!(
            "предмет {item_id}: stats.{slot}.duration_s должен быть > 0"
        )),
        None => warnings.push(format!("предмет {item_id}: stats.{slot} без duration_s")),
    }
    match buff.get("max_stacks").and_then(num) {
        Some(stacks) if stacks >= 1.0 => {}
        Some(_) => warnings.push(format!(
            "предмет {item_id}: stats.{slot}.max_stacks должен быть >= 1"
        )),
        None => warnings.push(format!("предмет {item_id}: stats.{slot} без max_stacks")),
    }
}

/// Эффект на цель (`stats.effect`): stun/root/bleed/burn + длительность;
/// bleed/burn требуют урон (`value`), burn — период (>= 0.5 с).
fn validate_item_effect(
    stats: &serde_json::Map<String, serde_json::Value>,
    item_id: &str,
    warnings: &mut Vec<String>,
) {
    let Some(effect) = stats.get("effect") else {
        return;
    };
    let serde_json::Value::Object(effect) = effect else {
        warnings.push(format!(
            "предмет {item_id}: stats.effect должен быть объектом"
        ));
        return;
    };
    let kind = effect.get("kind").and_then(|value| value.as_str());
    match kind {
        Some(kind) if EFFECT_KINDS.contains(&kind) => {
            if matches!(kind, "bleed" | "burn") {
                match effect.get("value").and_then(num) {
                    Some(value) if value >= 1.0 => {}
                    Some(_) => warnings.push(format!(
                        "предмет {item_id}: stats.effect.{kind} — value должен быть >= 1"
                    )),
                    None => {
                        warnings.push(format!("предмет {item_id}: stats.effect.{kind} без value"))
                    }
                }
            }
            if kind == "burn" {
                if let Some(period) = effect.get("period_s").and_then(num) {
                    if period < 0.5 {
                        warnings.push(format!(
                            "предмет {item_id}: stats.effect.burn.period_s должен быть >= 0.5"
                        ));
                    }
                }
            }
        }
        Some(kind) => warnings.push(format!(
            "предмет {item_id}: stats.effect — неизвестный вид {kind}"
        )),
        None => warnings.push(format!("предмет {item_id}: stats.effect без kind")),
    }
    match effect.get("duration_s").and_then(num) {
        Some(duration) if duration > 0.0 => {}
        Some(_) => warnings.push(format!(
            "предмет {item_id}: stats.effect.duration_s должен быть > 0"
        )),
        None => warnings.push(format!("предмет {item_id}: stats.effect без duration_s")),
    }
}

/// Валидация предметов (§1.1 docs/data-model.md, ADR-010): редкость, слот,
/// бонусы, статы рун, автобаффы, эффекты, требования, сокеты.
///
/// Только предупреждения: серверные правила проверяются тестами
/// (`items_content_is_valid`), редактору важно подсветить контент-мейкеру
/// сомнительные значения, не блокируя сохранение WIP-контента.
pub fn validate_items(content: &GameContent) -> Vec<String> {
    use std::collections::HashSet;

    let mut warnings = Vec::new();
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

    for item in &content.items {
        let item_id = item.id.as_str();

        if !ITEM_RARITIES.contains(&item.rarity.as_str()) {
            warnings.push(format!(
                "предмет {item_id}: неизвестная редкость {}",
                item.rarity
            ));
        }
        if let Some(slot) = &item.slot {
            if !ITEM_SLOTS.contains(&slot.as_str()) {
                warnings.push(format!("предмет {item_id}: неизвестный слот {slot}"));
            }
        }
        if let Some(skill) = &item.skill {
            if !skill_ids.contains(skill.as_str()) {
                warnings.push(format!("предмет {item_id}: неизвестный навык {skill}"));
            }
        }

        // Требования: только известные атрибуты/профессии.
        if let Some(requires) = &item.requires {
            for attr in requires.attr.keys() {
                if !ITEM_ATTRS.contains(&attr.as_str()) {
                    warnings.push(format!(
                        "предмет {item_id}: requires.attr — неизвестный атрибут {attr}"
                    ));
                }
            }
            if let Some(profession) = &requires.profession {
                if !profession_ids.contains(profession.as_str()) {
                    warnings.push(format!(
                        "предмет {item_id}: requires.profession — неизвестная профессия {profession}"
                    ));
                }
            }
        }

        // Бонусы (ADR-010): kind/id/value/pct, уникальность id в предмете.
        let mut seen_bonuses: HashSet<&str> = HashSet::new();
        for bonus in &item.bonuses {
            if !seen_bonuses.insert(bonus.id.as_str()) {
                warnings.push(format!(
                    "предмет {item_id}: дубликат id бонуса {}",
                    bonus.id
                ));
            }
            match bonus.kind.as_str() {
                "attr" if !ITEM_ATTRS.contains(&bonus.id.as_str()) => warnings.push(format!(
                    "предмет {item_id}: бонус attr — неизвестный атрибут {}",
                    bonus.id
                )),
                "param" if !ITEM_PARAMS.contains(&bonus.id.as_str()) => warnings.push(format!(
                    "предмет {item_id}: бонус param — неизвестный параметр {}",
                    bonus.id
                )),
                "skill" if !skill_ids.contains(bonus.id.as_str()) => warnings.push(format!(
                    "предмет {item_id}: бонус skill — неизвестный навык {}",
                    bonus.id
                )),
                "attr" | "param" | "skill" => {}
                kind => warnings.push(format!("предмет {item_id}: бонус — неизвестный вид {kind}")),
            }
            if bonus.value == 0 {
                warnings.push(format!(
                    "предмет {item_id}: бонус {} — значение не может быть 0",
                    bonus.id
                ));
            }
            if bonus.pct && bonus.kind != "param" {
                warnings.push(format!(
                    "предмет {item_id}: бонус {} — pct допустим только для param",
                    bonus.id
                ));
            }
        }

        // Броня/оружие/посох с сокетами обязаны объяснить, сколько из них
        // принимают руны (ADR-012).
        if matches!(item.item_type.as_str(), "weapon" | "staff")
            && item.sockets > 0
            && item.rune_sockets.is_none()
        {
            warnings.push(format!(
                "предмет {item_id}: sockets={} без rune_sockets (ADR-012)",
                item.sockets
            ));
        }

        let stats = &item.stats;
        for key in stats.keys() {
            if !ITEM_STAT_KEYS.contains(&key.as_str()) {
                warnings.push(format!(
                    "предмет {item_id}: неизвестная характеристика stats.{key}"
                ));
            }
        }
        if item.item_type == "rune" {
            match stats.get("glyph").and_then(|value| value.as_str()) {
                Some(glyph)
                    if !glyph.is_empty() && glyph.chars().all(|ch| ch.is_ascii_alphabetic()) => {}
                Some(_) => warnings.push(format!(
                    "предмет {item_id}: руна — глиф должен быть непустым набором ascii-букв"
                )),
                None => warnings.push(format!("предмет {item_id}: у руны отсутствует stats.glyph")),
            }
        }
        if let Some(school) = stats.get("school").and_then(|value| value.as_str()) {
            if !SPELL_SCHOOLS.contains(&school) {
                warnings.push(format!(
                    "предмет {item_id}: stats.school — неизвестная школа {school}"
                ));
            }
        }
        if let Some(class) = stats.get("class").and_then(|value| value.as_str()) {
            if !ITEM_CLASSES.contains(&class) {
                warnings.push(format!(
                    "предмет {item_id}: stats.class — неизвестный класс {class}"
                ));
            }
        }
        if let Some(attr) = stats.get("attr").and_then(|value| value.as_str()) {
            if !ITEM_ATTRS.contains(&attr) {
                warnings.push(format!(
                    "предмет {item_id}: stats.attr — неизвестный атрибут {attr}"
                ));
            }
        }
        validate_item_params(stats, item_id, &mut warnings);
        validate_item_buff(stats, "buff", item_id, &mut warnings);
        validate_item_buff(stats, "buff2", item_id, &mut warnings);
        validate_item_effect(stats, item_id, &mut warnings);
    }

    warnings
}

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

    /// E6.2: предметы репозитория проходят структурную валидацию.
    #[test]
    fn repository_items_are_clean() {
        let warnings = validate_items(&repo_content());
        assert!(warnings.is_empty(), "предметы: {warnings:?}");
    }

    /// E6.2: битые бонусы/статы/руны — предупреждения (не ошибки).
    #[test]
    fn broken_item_rules_are_warnings() {
        let mut content = repo_content();
        let axe = content
            .items
            .iter()
            .position(|item| item.id == "drakkar_axe")
            .expect("drakkar_axe");
        content.items[axe].rarity = "mythic".into();
        content.items[axe].bonuses[0].kind = "talent".into();
        content.items[axe].bonuses[1].value = 0;
        content.items[axe].bonuses[1].pct = true;
        let rune = content
            .items
            .iter()
            .position(|item| item.item_type == "rune")
            .expect("руна");
        content.items[rune].stats.remove("glyph");
        content.items[rune].stats.insert(
            "effect".into(),
            serde_json::json!({"kind": "burn", "duration_s": 6}),
        );
        let warnings = validate_items(&content);
        let joined = warnings.join("; ");
        for needle in ["mythic", "talent", "0", "glyph", "burn"] {
            assert!(joined.contains(needle), "нет {needle}: {joined}");
        }
    }
}

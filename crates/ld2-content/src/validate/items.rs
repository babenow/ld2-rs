//! Схема предметов: константы и проверки полей.

use super::*;

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

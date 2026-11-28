//! Руны: срабатывание, заряды, подписи параметров сокетов (M18).
use crate::consts::{EFFECT_BLEED, EFFECT_ROOT, EFFECT_STUN, ITEM_TYPE_GEM};

const PERCENT_DIVISOR: f64 = 100.0;

/// M18/F58d (ADR-012 §5): сработал ли авто-приём руны — `chance`
/// (0..1 из контента `stats.chance`) против броска `roll` [0,1).
/// Чистая функция: детерминирована юнит-тестами, бросок — снаружи.
pub fn rune_proc_roll(chance: f64, roll: f64) -> bool {
    chance > 0.0 && roll < chance
}

/// M18/F58f: русские названия параметров для серверных строк (§8:
/// сервер — истина; дубль клиентского `param_label`, но строки формирует
/// сервер при инкрустации).
fn param_title(id: &str) -> &str {
    match id {
        "accuracy" => "Точность",
        "crit_chance" => "Крит. шанс",
        "crit_damage" => "Крит. урон",
        "armor" => "Броня",
        "dodge" => "Уклон",
        "damage" => "Урон",
        other => other,
    }
}

/// M18/F58f: русские названия атрибутов для самоцветов (по именам
/// контента: «Кровавик» и т.д.).
fn attr_title(id: &str) -> &str {
    match id {
        "str" => "Сила",
        "dex" => "Ловкость",
        "vit" => "Стойкость",
        "int" => "Разум",
        other => other,
    }
}

/// M18/F58f (ADR-012): подробности занятого слота — что даёт вставка
/// (строки для окна информации; сервер формирует при инкрустации,
/// §8 — сервер-истина). `item_type` — `rune`|`gem` (тип вставки),
/// `stats` — параметры определения.
pub fn slot_detail_lines(item_type: &str, stats: &serde_json::Value) -> Vec<String> {
    let mut lines = Vec::new();
    if item_type == ITEM_TYPE_GEM {
        // Самоцвет: атрибут из контента (str/dex/vit/int + value).
        if let (Some(attr), Some(value)) = (
            stats.get("attr").and_then(|v| v.as_str()),
            stats.get("value").and_then(|v| v.as_i64()),
        ) {
            lines.push(format!("+{value} {}", attr_title(attr)));
        }
        if let (Some(param), Some(value)) = (
            stats.get("param").and_then(|v| v.as_str()),
            stats.get("value").and_then(|v| v.as_i64()),
        ) {
            lines.push(format!("+{value} {}", param_title(param)));
        }
        return lines;
    }
    // Руна: урон, шанс авто-приёма, бафф, эффект, трата.
    if let Some(damage) = stats.get("damage").and_then(|v| v.as_i64()) {
        if damage > 0 {
            lines.push(format!("+{damage} к урону"));
        }
    }
    if let Some(chance) = stats.get("chance").and_then(|v| v.as_f64()) {
        if chance > 0.0 {
            lines.push(format!(
                "шанс приёма {}%",
                (chance * PERCENT_DIVISOR).round()
            ));
        }
    }
    if let Some(buff) = stats.get("buff").and_then(|v| v.as_object()) {
        let param = buff.get("param").and_then(|v| v.as_str()).unwrap_or("");
        let value = buff.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
        let duration = buff.get("duration_s").and_then(|v| v.as_i64()).unwrap_or(0);
        let stacks = buff.get("max_stacks").and_then(|v| v.as_i64()).unwrap_or(1);
        lines.push(format!(
            "{}: +{} на {} с, до {} стак{}",
            param_title(param),
            value,
            duration,
            stacks,
            if stacks == 1 { "а" } else { "ов" }
        ));
    }
    if let Some(effect) = stats.get("effect").and_then(|v| v.as_object()) {
        let kind = effect.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let duration = effect
            .get("duration_s")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        match kind {
            EFFECT_BLEED => {
                let dps = effect.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                lines.push(format!("кровотечение: {dps}/сек, {duration} с"));
            }
            EFFECT_STUN => lines.push(format!("оглушение на {duration} с")),
            EFFECT_ROOT => lines.push(format!("опутывание на {duration} с")),
            _ => {}
        }
    }
    if let Some(cost) = stats.get("cost").and_then(|v| v.as_i64()) {
        if cost > 1 {
            lines.push(format!("трата {cost} зарядов"));
        }
    }
    lines
}

/// M18/F58c (ADR-012 §4): затрата зарядов руны за одно применение —
/// поле параметров руны `cost` (по умолчанию 1, минимум 1: нулевая
/// затрата давала бы вечную руну). Применяется и при касте, и при
/// авто-приёме (F58d) — одна точка чтения.
pub fn rune_charge_cost(stats: &serde_json::Value) -> i64 {
    stats
        .get("cost")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(1)
        .max(1)
}

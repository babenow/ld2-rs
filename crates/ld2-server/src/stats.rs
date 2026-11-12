//! M15/F26: производные характеристики персонажа — вкладка «Параметры»
//! окна персонажа (план F24–F27).
//!
//! Формулы — из GDD `docs/skills.md` §1 (роль атрибутов), §A/B (эффекты
//! боевых/защитных навыков **за ранг**) + статов экипировки
//! (`stats.dmg` вида "min..max", `stats.armor`, `stats.magic`);
//! чистые функции с юнит-тестами, базовые шансы — константы модуля
//! (баланс-решения зафиксированы в тестах).
//!
//! **Влияние на бой не входит** — боевой `crit` в тике остаётся
//! отдельным решением (урон по-прежнему из `gameplay.combat.base_damage`).
//!
//! Оружейные `%` (§A: «+2% урона мечом за ранг» и т.п.) читаются из
//! `ItemSnapshot.extra["skill"]` — вклад активируется, когда контент
//! станет передавать `skill` предмета в снапшот (сейчас extra пуст → 0).

use ld2_protocol::types::{DerivedParams, ItemSnapshot, SkillSnapshot};
use std::collections::BTreeMap;

/// Эффекты оружейных навыков §A за ранг: (урон %, крит %, уклон %).
const WEAPON_EFFECTS: &[(&str, f64, f64, f64)] = &[
    ("sword", 2.0, 0.0, 0.0),
    ("axe", 2.0, 0.0, 0.0),
    ("polearm", 2.0, 0.0, 0.0),
    ("bow", 2.0, 0.5, 0.0),
    ("dagger", 1.5, 0.7, 0.0),
    ("twohand", 3.0, 0.0, -0.3),
    ("unarmed", 2.0, 0.0, 0.5),
];

fn attr(attrs: &BTreeMap<String, i64>, id: &str) -> i64 {
    attrs.get(id).copied().unwrap_or(0)
}

/// Ранг навыка из снапшота (0 — не изучен).
fn rank(skills: &[SkillSnapshot], id: &str) -> i64 {
    skills
        .iter()
        .find(|s| s.id == id)
        .map(|s| i64::from(s.rank))
        .unwrap_or(0)
}

/// `stats.dmg = "min..max"` снаряжённого оружия; иначе боевая база
/// конфига (в снапшоте оружие может отсутствовать — стартовый кулак).
fn weapon_damage(equipped: &[ItemSnapshot], base: i64) -> (i64, i64) {
    for item in equipped {
        if let Some(raw) = item.stats.get("dmg").and_then(|v| v.as_str()) {
            if let Some((a, b)) = raw.split_once("..") {
                if let (Ok(min), Ok(max)) = (a.trim().parse::<i64>(), b.trim().parse::<i64>()) {
                    return (min, max);
                }
            }
        }
    }
    (base, base)
}

/// Навык снаряжённого оружия из `extra["skill"]` (см. док модуля).
fn equipped_weapon_skill(equipped: &[ItemSnapshot]) -> Option<&str> {
    equipped
        .iter()
        .find_map(|i| i.extra.get("skill").and_then(|v| v.as_str()))
}

/// M15/F26: все 16 параметров окна «Параметры» по данным снапшота.
pub fn derive(
    attrs: &BTreeMap<String, i64>,
    skills: &[SkillSnapshot],
    equipped: &[ItemSnapshot],
    base_damage: i64,
) -> DerivedParams {
    let (w_min, w_max) = weapon_damage(equipped, base_damage);
    let (strg, dex, vit, int) = (
        attr(attrs, "str"),
        attr(attrs, "dex"),
        attr(attrs, "vit"),
        attr(attrs, "int"),
    );

    // Вклад снаряжённого оружейного навыка (§A — в процентах за ранг).
    let (mut dmg_pct, mut crit_pct, mut dodge_pct) = (0.0f64, 0.0f64, 0.0f64);
    if let Some(skill_id) = equipped_weapon_skill(equipped) {
        if let Some(&(_, d, c, u)) = WEAPON_EFFECTS.iter().find(|(id, ..)| *id == skill_id) {
            let r = rank(skills, skill_id) as f64;
            dmg_pct = d * r;
            crit_pct = c * r;
            dodge_pct = u * r;
        }
    }

    // ── физические ──
    let dmg_mult = 1.0 + dmg_pct / 100.0;
    let damage_min = (((w_min + strg / 2) as f64) * dmg_mult).round() as i64;
    let damage_max = (((w_max + strg / 2) as f64) * dmg_mult).round() as i64;
    // Точность: ловкость (GDD §1) — база под «стартовый» 80 %.
    let accuracy = (75 + dex / 2).clamp(1, 99);
    let crit_chance = ((5.0 + dex as f64 * 0.3 + crit_pct).round() as i64).clamp(0, 100);
    let crit_damage = 50 + dex / 5;
    // Броня — сумма статов экипировки (контент брони пока пуст → 0).
    let armor: i64 = equipped
        .iter()
        .filter_map(|i| i.stats.get("armor").and_then(|v| v.as_i64()))
        .sum();
    // Уклон: база + dex + защитные навыки §B (за ранг) + вклад оружия.
    let dodge = ((3.0
        + dex as f64 * 0.4
        + (rank(skills, "evasion") + rank(skills, "armor_light")) as f64
        + dodge_pct)
        .round() as i64)
        .clamp(0, 60);
    // Парирование: навык `parring` (контент-ид) + dex; контрудар ~½ урона.
    let parry_chance = (5 + rank(skills, "parring") + dex / 10).clamp(0, 60);
    let parry_min = w_min / 2 + strg / 4;
    let parry_max = w_max / 2 + strg / 4;
    // Блок: §B «+2% шанса полного блока за ранг»; снижение урона —
    // щит-предмет (в контенте пока нет → 0..0).
    let block_chance = (rank(skills, "block") * 2).clamp(0, 60);

    // ── магические ──
    let staff_magic: i64 = equipped
        .iter()
        .filter_map(|i| i.stats.get("magic").and_then(|v| v.as_i64()))
        .sum();
    let magic_defense = int + rank(skills, "m_armor") * 2 + staff_magic;
    let magic_dodge = ((int * 3 / 10) + rank(skills, "m_evasion")).clamp(0, 60);
    let magic_resist = (vit / 2 + rank(skills, "m_armor")).clamp(0, 60);
    let magic_dispel = (int / 2 + rank(skills, "m_dispel")).clamp(0, 99);
    let magic_crit_damage = 50 + int / 5;
    let magic_crit_chance = (5 + int * 3 / 10 + rank(skills, "arcana") / 2).clamp(0, 100);

    DerivedParams {
        accuracy,
        damage_min,
        damage_max,
        crit_chance,
        crit_damage,
        armor,
        dodge,
        parry_min,
        parry_max,
        parry_chance,
        block_min: 0,
        block_max: 0,
        block_chance,
        magic_defense,
        magic_dodge,
        magic_resist,
        magic_dispel,
        magic_crit_damage,
        magic_crit_chance,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs10() -> BTreeMap<String, i64> {
        ["str", "dex", "vit", "int"]
            .iter()
            .map(|k| (k.to_string(), 10i64))
            .collect()
    }

    fn skill(id: &str, rank: u32) -> SkillSnapshot {
        SkillSnapshot {
            id: id.into(),
            name: None,
            description: None,
            group: None,
            rank,
            xp: 0,
            xp_to_next: 0,
        }
    }

    fn item(stats: serde_json::Value, extra: serde_json::Value) -> ItemSnapshot {
        ItemSnapshot {
            uid: "t1".into(),
            item_type: "weapon".into(),
            name: "Т".into(),
            rarity: "common".into(),
            upgrade: 0,
            stats: stats.as_object().cloned().unwrap_or_default(),
            requires: None,
            sockets: None,
            qty: None,
            gold_value: None,
            extra: extra.as_object().cloned().unwrap_or_default(),
        }
    }

    /// M15/F26: стартовый персонаж (все атрибуты 10, без навыков и
    /// экипировки, база урона 6) — точные значения по формулам GDD.
    #[test]
    fn start_params_match_gdd() {
        let d = derive(&attrs10(), &[], &[], 6);
        assert_eq!((d.damage_min, d.damage_max), (11, 11), "6 + str/2");
        assert_eq!(d.accuracy, 80, "75 + dex/2");
        assert_eq!(
            (d.crit_chance, d.crit_damage),
            (8, 52),
            "5+dex·0.3 / 50+dex/5"
        );
        assert_eq!((d.armor, d.dodge), (0, 7), "без брони / 3+dex·0.4");
        assert_eq!((d.parry_min, d.parry_max), (5, 5), "w/2 + str/4");
        assert_eq!(d.parry_chance, 6, "5 + dex/10");
        assert_eq!((d.block_min, d.block_max, d.block_chance), (0, 0, 0));
        assert_eq!(
            (
                d.magic_defense,
                d.magic_dodge,
                d.magic_resist,
                d.magic_dispel
            ),
            (10, 3, 5, 5)
        );
        assert_eq!((d.magic_crit_damage, d.magic_crit_chance), (52, 8));
    }

    /// M15/F26: оружие (dmg 2..4, extra skill dagger р.3), навыки §B
    /// и магнавыки меняют параметры по формулам.
    #[test]
    fn weapon_and_skills_change_params() {
        let dagger = item(
            serde_json::json!({"dmg": "2..4"}),
            serde_json::json!({"skill": "dagger"}),
        );
        let skills = vec![
            skill("dagger", 3),
            skill("evasion", 5),
            skill("block", 2),
            skill("m_dispel", 4),
            skill("arcana", 2),
        ];
        let d = derive(&attrs10(), &skills, &[dagger], 6);
        // (2 + 5) × 1.045 = 7.315 → 7; (4 + 5) × 1.045 = 9.405 → 9.
        assert_eq!((d.damage_min, d.damage_max), (7, 9), "dagger +1.5%/ранг");
        // 5 + 10·0.3 + 0.7·3 = 10.1 → 10.
        assert_eq!(d.crit_chance, 10, "dagger +0.7% крита/ранг");
        // 3 + dex·0.4(4) + evasion 5 + вклад кинжала 0 = 12
        // (бонус +0.5/ранг — у `unarmed`, не у кинжала).
        assert_eq!(d.dodge, 12, "уклон с evasion");
        assert_eq!(d.parry_chance, 6, "parring не изучен");
        assert_eq!(d.block_chance, 4, "block +2%/ранг");
        assert_eq!(d.magic_dispel, 9, "int/2 + m_dispel 4");
        assert_eq!(d.magic_crit_chance, 9, "5 + 3 + arcana/2");
    }

    /// M15/F26: парсер dmg, кривые строки и клампы шансов.
    #[test]
    fn dmg_parse_and_clamps() {
        // Кривой формат → боевая база.
        let broken = item(
            serde_json::json!({"dmg": "не число"}),
            serde_json::json!({}),
        );
        assert_eq!(weapon_damage(&[broken], 6), (6, 6));
        // Огромная ловкость не даёт > 99 % точности.
        let mut attrs = attrs10();
        attrs.insert("dex".into(), 500);
        let d = derive(&attrs, &[], &[], 6);
        assert_eq!(d.accuracy, 99, "кламп точности");
        assert_eq!(d.dodge, 60, "кламп уклона");
        // Отрицательный dex (не бывает, но защита) → не ниже 0.
        let mut attrs = attrs10();
        attrs.insert("dex".into(), -100);
        let d = derive(&attrs, &[], &[], 6);
        assert!(
            d.dodge >= 0 && d.crit_chance >= 0,
            "нет отрицательных шансов"
        );
    }
}

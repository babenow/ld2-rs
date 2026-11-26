//! Опыт: уровни, профессии, награда за убийство (skills.md §2).

use super::*;

/// Порог ранга: `round(base_xp × growth^rank)` (skills.md §2.2, D25).
/// M21: порог **уровня** профессии: `round(base × growth^L)` — каждый
/// уровень дороже (согласованная формула `40 × 1.02^L`).
pub fn profession_level_xp(cfg: &crate::config::ProfessionsSection, level: u32) -> i64 {
    ((cfg.level_xp_base as f64) * cfg.level_xp_growth.powi(level as i32)).round() as i64
}

/// M21/M22: начислить XP профессии → **не больше одного уровня за
/// одно начисление** (указание заказчика), но **избыточный опыт
/// переносится на следующий уровень** (шкала может быть «полной» —
/// уровень даст следующее начисление; потери остатка нет). На
/// `max_level` опыт перестаёт копиться (→0). Ранг не меняется (его
/// даёт тренер или награда квеста). → `(xp, level)`.
pub fn profession_level_up(
    cfg: &crate::config::ProfessionsSection,
    xp: i64,
    level: u32,
    gain: i64,
) -> (i64, u32) {
    if level >= cfg.max_level {
        return (0, cfg.max_level);
    }
    let mut xp = (xp + gain).max(0);
    let mut level = level;
    let need = profession_level_xp(cfg, level);
    if xp >= need {
        xp -= need;
        level += 1;
        // Излишек остаётся в шкале (переносится), но второй уровень
        // за одно начисление не выдаётся.
    }
    if level >= cfg.max_level {
        xp = 0; // потолок: опыт не копится
    }
    (xp, level)
}

/// Снятие шкур: шанс **шкуры** (редкий дроп) растёт с уровнем
/// профессии: `min(60%, 1% + 2% × уровень)` — на старте практически
/// невозможно (ур.0 →1 %), потолок с ур.30. При провале — обрезки.
pub fn hide_chance(level: u32) -> f64 {
    (0.01 + 0.02 * f64::from(level)).min(0.6)
}

/// Снятие шкур: шанс **крита** — по рангу профессии (р1 →5 % …
/// р5 →25 %); при крите в лут сверх обычного результата падает
/// **+2 шкуры** текущего качества.
pub fn harvest_crit(rank: u32) -> f64 {
    f64::from(rank.clamp(1, 5)) * 0.05
}

/// Снятие шкур: опыт за «Снятие шкуры» (освежевание не даёт опыта) —
/// зависит от требуемого уровня животного: `база = 10 + треб ×1.5`,
/// далее множитель по разнице (уровень персонажа − требуемый):
/// −10 и ниже →×1.5, в пределах ±10 →×1.0, +10…+24 →×0.5,
/// +25 и выше →×0; **потолок40 за действие** (указание заказчика:
/// «максимум30-40»). Животное с требуемым уровнем выше персонажа на
/// 25+ разделать нельзя (гейт — на сервере).
pub fn harvest_xp_gain(required: u32, char_level: u32) -> i64 {
    let base = 10.0 + f64::from(required) * 1.5;
    let diff = f64::from(char_level) - f64::from(required);
    let mult = if diff <= -10.0 {
        1.5
    } else if diff <= 9.0 {
        1.0
    } else if diff <= 24.0 {
        0.5
    } else {
        0.0
    };
    ((base * mult).round() as i64).min(40)
}

/// M21: снимок профессии для wire (§6): `xp_to_next` — порог
/// **уровня** (0 на потолке), `extra` несёт `rank_span` и — до
/// высшего ранга — цену повышения у тренера (`rank_up_gold`).
pub fn profession_snapshot(
    cfg: &crate::config::ProfessionsSection,
    id: &str,
    xp: i64,
    rank: i32,
    level: u32,
) -> ld2_protocol::types::ProfessionSnapshot {
    let rank_u32 = rank.max(1) as u32;
    let xp_to_next = if level >= cfg.max_level {
        0
    } else {
        profession_level_xp(cfg, level)
    };
    let mut extra = serde_json::Map::new();
    extra.insert("rank_span".into(), serde_json::json!(cfg.rank_span));
    if rank_u32 < profession_rank_max(cfg) {
        extra.insert(
            "rank_up_gold".into(),
            serde_json::json!(profession_rank_up_cost(cfg, rank_u32)),
        );
    }
    ld2_protocol::types::ProfessionSnapshot {
        id: id.to_string(),
        rank: rank_u32,
        level,
        xp,
        xp_to_next,
        extra,
    }
}

/// M21: производный потолок ранга: `ceil(max_level / rank_span)` (=5).
pub fn profession_rank_max(cfg: &crate::config::ProfessionsSection) -> u32 {
    cfg.max_level.div_ceil(cfg.rank_span.max(1)).max(1)
}

/// M21: цена повышения ранга у тренера: `round(base × growth^ранг)`
/// (`200 × 1.5^ранг` →300/450/675/1013).
pub fn profession_rank_up_cost(cfg: &crate::config::ProfessionsSection, rank: u32) -> i64 {
    ((cfg.rank_up_gold_base as f64) * cfg.rank_up_gold_growth.powi(rank as i32)).round() as i64
}

pub fn xp_to_next(rank: u32, cfg: &SkillsSection) -> i64 {
    ((cfg.base_xp as f64) * cfg.growth.powi(rank as i32)).round() as i64
}

/// M15/F24: порог уровня: `XP = round(A×level² + B×level + C)` —
/// опыт перехода `level → level+1` (level ≥ 1).
pub fn level_xp_to_next(level: u32, cfg: &LevelSection) -> i64 {
    let l = f64::from(level.max(1));
    (cfg.a * l * l + cfg.b * l + cfg.c).round() as i64
}

/// M15/F24: уровень по общему опыту → `(level, xp в уровне, xp до
/// следующего)`. На `max_level` опыт копится, `xp_to_next = 0` (UI
/// прячет шкалу) — зерно потолка навыков.
pub fn level_for_total(total_xp: i64, cfg: &LevelSection) -> (u32, i64, i64) {
    let mut level = 1u32;
    let mut rest = total_xp.max(0);
    while level < cfg.max_level {
        let need = level_xp_to_next(level, cfg);
        if need <= 0 || rest < need {
            break;
        }
        rest -= need;
        level += 1;
    }
    if level >= cfg.max_level {
        (level, rest, 0)
    } else {
        (level, rest, level_xp_to_next(level, cfg))
    }
}

/// M31: группы, для которых ранг ограничен уровнем персонажа
/// (заказчик: боевые/защитные/магические — не более 2 за уровень).
pub fn level_capped_group(group: &str) -> bool {
    matches!(group, "weapon" | "defense" | "magic")
}

/// M31: потолок ранга навыка. Для боевых/защитных/магических —
/// `min(rank_max, ranks_per_level × уровень персонажа)`, для остальных
/// групп — только `rank_max`.
pub fn skill_rank_cap(cfg: &SkillsSection, group: &str, char_level: u32) -> u32 {
    if level_capped_group(group) {
        char_level
            .max(1)
            .saturating_mul(cfg.ranks_per_level.max(1))
            .min(cfg.rank_max)
    } else {
        cfg.rank_max
    }
}

/// Применить опыт убийства к навыку (skills.md §2.3): ранги растут, пока
/// хватает гейтов атрибутов; на гейте/потолке XP копится в текущем ранге
/// (гейты блокируют без ошибки — в отличие от вложения пула, §2.2).
/// Возвращает `(xp_в_ранге, ранг)`.
pub fn apply_kill_xp(
    xp: i64,
    rank: i32,
    amount: i64,
    cfg: &SkillsSection,
    max_attr: i64,
    rank_cap: u32,
) -> (i64, i32) {
    let mut xp = xp + amount;
    let mut rank = rank;
    loop {
        let need = xp_to_next(rank.max(0) as u32, cfg);
        if xp < need {
            break;
        }
        if rank >= rank_cap as i32 {
            break; // потолок: XP копится без роста ранга
        }
        let next = rank + 1;
        if let Some(gate) = cfg.gates.iter().find(|g| g.rank as i32 == next) {
            if max_attr < gate.attr {
                break; // гейт атрибута: стоп без ошибки
            }
        }
        xp -= need;
        rank = next;
    }
    (xp, rank)
}

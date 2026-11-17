//! Чистые игровые правила (без tokio/sqlx) — юнит-тесты (M12.5).

use crate::config::{LevelSection, SkillsSection, UpgradeSection};

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
) -> (i64, i32) {
    let mut xp = xp + amount;
    let mut rank = rank;
    loop {
        let need = xp_to_next(rank.max(0) as u32, cfg);
        if xp < need {
            break;
        }
        if rank >= cfg.rank_max as i32 {
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

/// Правило провала улучшения (`features."upgrade.rules"`, ФР-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeFailRule {
    /// Уровень не меняется.
    Keep,
    /// Уровень падает на 1 (не ниже 0).
    LevelDown,
    /// Предмет разрушается.
    Break,
}

impl UpgradeFailRule {
    /// Строковое значение capability из конфига; неизвестное → безопасное
    /// «без потери» (клиент capability-driven, §5).
    pub fn parse(s: &str) -> Self {
        match s {
            "level_down" => Self::LevelDown,
            "break_on_fail" => Self::Break,
            _ => Self::Keep,
        }
    }
}

/// Стоимость улучшения `current → current+1` (золото, D25 — формула из
/// конфига): `gold_base + gold_per_level × current`.
pub fn upgrade_cost(current: i64, cfg: &UpgradeSection) -> i64 {
    (cfg.gold_base + cfg.gold_per_level * current).max(0)
}

/// Шанс успеха с уровня `current` (`0..=1`): не растёт с уровнем.
pub fn upgrade_chance(current: i64, cfg: &UpgradeSection) -> f64 {
    (cfg.chance_base - cfg.chance_per_level * current as f64).clamp(0.0, 1.0)
}

/// M18/F54: шанс заточки с камнем усиления. Базовый шанс конфига
/// плюс **качество камня** (процентные пункты, `stats.quality`, может
/// быть отрицательным у малого камня) с динамическим затуханием
/// `1/(1+level)` — чем выше уровень, тем слабее бонус камня; сверху
/// множитель **рейтов сервера** (`rates.chance`). Итог клампится в 0..=1.
///
/// ```
/// # use ld2_server::systems::upgrade_chance_with_stone;
/// // качество ±5%, уровень 0 → ±5% к шансу; уровень 9 → ±0.5%.
/// assert!((upgrade_chance_with_stone(0.9, 5, 0, 1.0) - 0.95).abs() < 1e-9);
/// assert!((upgrade_chance_with_stone(0.9, -5, 0, 1.0) - 0.85).abs() < 1e-9);
/// // рейты 0 → шанс гарантированно падает.
/// assert_eq!(upgrade_chance_with_stone(0.9, 5, 0, 0.0), 0.0);
/// ```
pub fn upgrade_chance_with_stone(
    base: f64,
    stone_quality: i64,
    level: i64,
    rates_chance: f64,
) -> f64 {
    let stone_bonus = stone_quality as f64 / 100.0 / (1.0 + level.max(0) as f64);
    ((base + stone_bonus) * rates_chance).clamp(0.0, 1.0)
}

/// Бросок улучшения: `roll ∈ [0,1)` против шанса (v1: источник — время,
/// детерминизма не обещаем; тесты выставляют шанс 0/1).
pub fn upgrade_roll(now_ms: i64) -> f64 {
    // Взбитое время — без rand-зависимости (как в combat.flee).
    let mixed = now_ms.wrapping_mul(0x9E37_79B9_7F4A_7C15_u64 as i64);
    (mixed.unsigned_abs() % 10_000) as f64 / 10_000.0
}

/// Вклад нового преступления в общий срок (§10).
///
/// `T = 0` (первое преступление) → полный срок `t` — так согласован пример
/// §10 (murderer=21600 первым); иначе частичное сложение
/// `k·t·(1 − T/T_MAX)`. Возвращает итоговый общий срок
/// `T_new = min(T_MAX, T + вклад)` и **дельту** — сколько секунд
/// добавляется новому статусу (он живёт `delta` секунд).
pub fn crime_blend(t_cur: i64, t_new: i64, t_max: i64, k: f64) -> (i64, i64) {
    let t_cur = t_cur.max(0);
    let t_max = t_max.max(1).max(t_cur);
    let delta = if t_cur == 0 {
        // Первое преступление — без «скидки».
        t_new.max(0)
    } else {
        let ratio = (t_cur as f64 / t_max as f64).clamp(0.0, 1.0);
        (k * t_new.max(0) as f64 * (1.0 - ratio)).round().max(0.0) as i64
    };
    let total = (t_cur + delta).min(t_max);
    (total, (total - t_cur).max(0))
}

// ─── M17/F30e: ресурсы по классам (WoW-модель) ─────────────────────────────

/// Максимум ресурса по профилю: воин — ярость, лучник — энергия
/// (из `[gameplay.resource]`), остальные (мана) — колонка персонажа.
pub fn resource_max(profile: &str, res: &crate::config::ResourceSection, mp_max: i64) -> i64 {
    match profile {
        "warrior" => i64::from(res.rage_max),
        "archer" => i64::from(res.energy_max),
        _ => mp_max,
    }
}

/// Название ресурса в серверных сообщениях (родительный падеж):
/// «не хватает ярости/энергии/маны».
pub fn resource_name(profile: &str) -> &'static str {
    match profile {
        "warrior" => "ярости",
        "archer" => "энергии",
        _ => "маны",
    }
}

/// Прирост ярости за урон (воин): доля `rage_gain_pct`, округление
/// вверх; не-воин или нулевой урон → 0.
pub fn rage_gain(res: &crate::config::ResourceSection, profile: &str, damage: i64) -> i64 {
    if profile != "warrior" || damage <= 0 {
        return 0;
    }
    (damage as f64 * res.rage_gain_pct).ceil() as i64
}

/// M17/F30e: шаг ресурса онлайн-персонажа раз в секунду по профилю:
/// **воин** — ярость падает вне боя (в бою не трогаем), **лучник** —
/// энергия восстанавливается всегда (в бою и вне), **мана** — прежняя
/// модель §2.4 (вне боя полный бар за `regen_out_s`, в бою %/с).
/// `None` — изменений нет (в том числе при полном ресурсе).
pub fn resource_step(
    profile: &str,
    cur: i64,
    max: i64,
    in_combat: bool,
    cfg: &crate::config::Config,
) -> Option<i64> {
    let cur = cur.min(max);
    let new = match profile {
        "warrior" => {
            if in_combat {
                return None;
            }
            (cur as f64 - cfg.gameplay.resource.rage_decay_s)
                .max(0.0)
                .round() as i64
        }
        "archer" => {
            if cur >= max {
                return None;
            }
            let step = cfg.gameplay.resource.energy_regen_s.round() as i64;
            (cur + step).min(max)
        }
        _ => {
            if cur >= max {
                return None;
            }
            let mana = &cfg.gameplay.magic.mana;
            let delta = if in_combat {
                (max as f64 * mana.regen_in_pct_s / 100.0).ceil().max(1.0) as i64
            } else {
                (max as f64 / mana.regen_out_s.max(1) as f64)
                    .ceil()
                    .max(1.0) as i64
            };
            (cur + delta).min(max)
        }
    };
    (new != cur).then_some(new)
}

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
/// контента: «Самоцвет силы» и т.д.).
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
    if item_type == "gem" {
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
            lines.push(format!("шанс приёма {}%", (chance * 100.0).round()));
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
            "bleed" => {
                let dps = effect.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
                lines.push(format!("кровотечение: {dps}/сек, {duration} с"));
            }
            "stun" => lines.push(format!("оглушение на {duration} с")),
            "root" => lines.push(format!("опутывание на {duration} с")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SkillGate;

    /// M18/F58f: подробности слота — урон/шанс/бафф/эффект/трата у
    /// рун, атрибут у самоцветов; пустые params → пустой список.
    #[test]
    fn slot_detail_lines_describe_rune_and_gem() {
        // Руна с баффом и эффектом.
        let stats = serde_json::json!({
            "damage": 12, "chance": 0.3, "cost": 2,
            "buff": { "param": "accuracy", "value": 6, "duration_s": 12, "max_stacks": 3 },
            "effect": { "kind": "bleed", "value": 3, "duration_s": 6 }
        });
        let lines = slot_detail_lines("rune", &stats);
        assert_eq!(lines[0], "+12 к урону");
        assert_eq!(lines[1], "шанс приёма 30%");
        assert_eq!(lines[2], "Точность: +6 на 12 с, до 3 стаков");
        assert_eq!(lines[3], "кровотечение: 3/сек, 6 с");
        assert_eq!(lines[4], "трата 2 зарядов");

        // Стан и опутывание.
        let stun = serde_json::json!({ "effect": { "kind": "stun", "duration_s": 3 } });
        assert_eq!(slot_detail_lines("rune", &stun), vec!["оглушение на 3 с"]);
        let root = serde_json::json!({ "effect": { "kind": "root", "duration_s": 5 } });
        assert_eq!(slot_detail_lines("rune", &root), vec!["опутывание на 5 с"]);

        // Самоцвет — атрибут.
        let gem = serde_json::json!({ "attr": "str", "value": 3 });
        assert_eq!(slot_detail_lines("gem", &gem), vec!["+3 Сила"]);

        // Минимальная руна (только урон) и пустые params.
        assert_eq!(
            slot_detail_lines("rune", &serde_json::json!({ "damage": 5 })),
            vec!["+5 к урону"]
        );
        assert!(slot_detail_lines("rune", &serde_json::json!({})).is_empty());
    }

    /// M18/F58d: шанс авто-приёма — 0 никогда, 1 всегда, граница
    /// строго `< chance` (roll = chance не срабатывает).
    #[test]
    fn rune_proc_roll_uses_chance_bounds() {
        assert!(!rune_proc_roll(0.0, 0.0), "ноль шанса не срабатывает");
        assert!(rune_proc_roll(1.0, 0.0), "гарантия");
        assert!(rune_proc_roll(1.0, 0.999), "гарантия у верхней границы");
        assert!(rune_proc_roll(0.5, 0.49), "ниже шанса");
        assert!(!rune_proc_roll(0.5, 0.5), "равно шансу — промах");
        assert!(!rune_proc_roll(-1.0, 0.0), "некорректный шанс");
        assert!(!rune_proc_roll(f64::NAN, 0.0), "NaN не срабатывает");
    }

    /// M18/F58c: затрата зарядов — поле `cost` руны, дефолт 1, кламп ≥ 1.
    #[test]
    fn rune_charge_cost_reads_rune_param() {
        let stats = |json: &str| serde_json::from_str::<serde_json::Value>(json).expect("json");
        assert_eq!(rune_charge_cost(&serde_json::json!({})), 1, "без поля — 1");
        assert_eq!(
            rune_charge_cost(&stats(r#"{"cost": 3}"#)),
            3,
            "явная затрата"
        );
        assert_eq!(
            rune_charge_cost(&stats(r#"{"cost": 0}"#)),
            1,
            "0 → кламп 1: руна не бессмысленна"
        );
        assert_eq!(rune_charge_cost(&stats(r#"{"cost": -5}"#)), 1);
    }

    fn cfg() -> SkillsSection {
        SkillsSection {
            base_xp: 120,
            growth: 1.32,
            rank_max: 30,
            gates: vec![
                SkillGate { rank: 10, attr: 12 },
                SkillGate { rank: 20, attr: 18 },
                SkillGate { rank: 30, attr: 25 },
            ],
            kill_xp_base: 8,
            kill_xp_per_difficulty: 6,
            active: Default::default(),
        }
    }

    /// Конфиг профессий для юнит-тестов M21 (значения из example).
    fn prof_cfg() -> crate::config::ProfessionsSection {
        crate::config::ProfessionsSection {
            xp_per_craft_base: 40,
            xp_per_craft_per_rank: 25,
            fail_xp_ratio: 0.2,
            level_xp_base: 40,
            level_xp_growth: 1.02,
            max_level: 250,
            rank_span: 50,
            rank_up_gold_base: 200,
            rank_up_gold_growth: 1.5,
        }
    }

    /// M22: шанс шкуры растёт с уровнем (на старте почти ноль),
    /// потолок60 %; крит — по рангу5…25 %.
    #[test]
    fn hide_chance_and_crit_scale() {
        assert!((hide_chance(0) - 0.01).abs() < f64::EPSILON, "ур0 —1%");
        assert!((hide_chance(10) - 0.21).abs() < 1e-9, "ур10 —21%");
        assert!((hide_chance(25) - 0.51).abs() < 1e-9, "ур25 —51%");
        assert!((hide_chance(30) - 0.6).abs() < 1e-9, "потолок60%");
        assert!((hide_chance(250) - 0.6).abs() < 1e-9, "выше потолка");
        assert!((harvest_crit(1) - 0.05).abs() < 1e-9, "р1 —5%");
        assert!((harvest_crit(5) - 0.25).abs() < 1e-9, "р5 —25%");
        assert!((harvest_crit(9) - 0.25).abs() < 1e-9, "ранг зажат5");
    }

    /// M22: опыт за снятие шкуры — база «10 + треб ×1.5» × множитель
    /// по разнице уровней, **потолок40 за действие**
    /// (таблица docs/professions.md §3).
    #[test]
    fn harvest_xp_gain_by_level_diff() {
        // diff −5 (ниже требования) →×1: round(10 +5×1.5) = 18.
        assert_eq!(harvest_xp_gain(5, 0), 18);
        // ровно на уровне →×1.
        assert_eq!(harvest_xp_gain(5, 5), 18);
        // diff −10 и ниже →×1.5: round(25×1.5) = 38.
        assert_eq!(harvest_xp_gain(10, 0), 38);
        // diff +10…+24 →×0.5.
        assert_eq!(harvest_xp_gain(5, 15), 9);
        assert_eq!(harvest_xp_gain(5, 29), 9);
        // diff +25 и выше →×0.
        assert_eq!(harvest_xp_gain(5, 30), 0);
        assert_eq!(harvest_xp_gain(1, 26), 0);
        // Потолок: сколько бы ни было «в плюс», не больше40.
        assert_eq!(harvest_xp_gain(40, 0), 40, "потолок40");
        assert_eq!(harvest_xp_gain(200, 0), 40, "потолок40");
        assert_eq!(
            harvest_xp_gain(200, 200),
            40,
            "на уровне — база выше потолка"
        );
    }

    /// M21: формула уровня `round(40 × 1.02^L)` — растёт монотонно.
    #[test]
    fn profession_level_curve_grows() {
        let cfg = prof_cfg();
        assert_eq!(profession_level_xp(&cfg, 0), 40, "L0");
        assert_eq!(profession_level_xp(&cfg, 1), 41, "L1");
        assert_eq!(profession_level_xp(&cfg, 50), 108, "L50");
        assert_eq!(profession_level_xp(&cfg, 100), 290, "L100");
        let (a, b) = (
            profession_level_xp(&cfg, 150),
            profession_level_xp(&cfg, 151),
        );
        assert!(b > a, "каждый уровень дороже: {a} → {b}");
        let top = profession_level_xp(&cfg, 249);
        assert!((5_000..=6_000).contains(&top), "потолочный уровень: {top}");
    }

    /// M21/M22: не больше **одного уровня** за одно начисление, но
    /// **избыточный опыт переносится** на следующий уровень; на
    /// max_level опыт не копится.
    #[test]
    fn profession_level_up_one_level_and_carry() {
        let cfg = prof_cfg();
        // 100 XP при пороге40 → ровно +1 уровень, остаток60 — в шкале.
        assert_eq!(profession_level_up(&cfg, 0, 0, 100), (60, 1));
        // Без заполнения шкалы — только опыт.
        assert_eq!(profession_level_up(&cfg, 10, 0, 20), (30, 0));
        // Излишек (641 −41 =600) целиком переносится, но уровней
        // за начисление всё равно один.
        assert_eq!(profession_level_up(&cfg, 41, 1, 600), (600, 2));
        // Следующее начисление: шкала была полной → снова +1.
        assert_eq!(profession_level_up(&cfg, 600, 2, 10), (600 - 42 + 10, 3));
        // Потолок: на L250 опыт не копится.
        assert_eq!(profession_level_up(&cfg, 40, 249, 10_000), (0, 250));
        assert_eq!(profession_level_up(&cfg, 7, 250, 100), (0, 250));
        // Отрицательный gain не уводит XP в минус.
        assert_eq!(profession_level_up(&cfg, 5, 1, -100), (0, 1));
    }

    /// M21: ранги1…5 (границы по 50 уровней), цена ранга — геометрия
    /// `200 × 1.5^ранг` (300/450/675/1013).
    #[test]
    fn profession_rank_max_and_up_cost() {
        let cfg = prof_cfg();
        assert_eq!(profession_rank_max(&cfg), 5, "потолок ранга");
        assert_eq!(profession_rank_up_cost(&cfg, 1), 300, "ранг1→2");
        assert_eq!(profession_rank_up_cost(&cfg, 2), 450, "ранг2→3");
        assert_eq!(profession_rank_up_cost(&cfg, 3), 675, "ранг3→4");
        assert_eq!(profession_rank_up_cost(&cfg, 4), 1_013, "ранг4→5");
    }

    #[test]
    fn xp_to_next_follows_config_formula() {
        let c = cfg();
        assert_eq!(xp_to_next(0, &c), 120, "r0 → base_xp");
        // Формула из конфига — истина (примерные числа в §2.2 документа
        // расходятся с формулой — сервер следует конфигу).
        assert_eq!(xp_to_next(5, &c), 481, "round(120 × 1.32^5)");
        assert!(
            (1..20).all(|r| xp_to_next(r, &c) < xp_to_next(r + 1, &c)),
            "пороги монотонно растут"
        );
    }

    fn level_cfg() -> LevelSection {
        LevelSection {
            a: 50.0,
            b: 50.0,
            c: 0.0,
            max_level: 60,
        }
    }

    /// M15/F24: квадратичная кривая — round(A×level²+B×level+C).
    #[test]
    fn level_curve_follows_config() {
        let c = level_cfg();
        assert_eq!(level_xp_to_next(1, &c), 100, "50 + 50 + 0");
        assert_eq!(level_xp_to_next(2, &c), 300, "50·4 + 50·2");
        assert_eq!(level_xp_to_next(3, &c), 600, "50·9 + 50·3");
        assert!(
            (1..60).all(|l| level_xp_to_next(l, &c) < level_xp_to_next(l + 1, &c)),
            "пороги монотонно растут"
        );
    }

    /// M15/F24: разбор общего опыта: уровень / прогресс / порог.
    #[test]
    fn level_for_total_walks_thresholds() {
        let c = level_cfg();
        assert_eq!(level_for_total(0, &c), (1, 0, 100));
        assert_eq!(level_for_total(99, &c), (1, 99, 100));
        assert_eq!(level_for_total(100, &c), (2, 0, 300));
        assert_eq!(level_for_total(399, &c), (2, 299, 300));
        assert_eq!(level_for_total(400, &c), (3, 0, 600));
        // Потолок: сумма квадратичных порогов до 60 ≈ 3.6M — берём i64::MAX.
        let (lvl, xp, next) = level_for_total(i64::MAX, &c);
        assert_eq!(lvl, 60, "потолок max_level");
        assert_eq!(next, 0, "на потолке шкалы нет");
        assert!(xp > 0, "опыт копится на потолке");
        assert_eq!(level_for_total(-5, &c), (1, 0, 100), "защита от минуса");
    }

    #[test]
    fn kill_xp_levels_up_until_gate() {
        let c = cfg();
        // Атрибутов 10 < 12 → гейт ранга 10 останавливает рост, XP копится.
        let (xp, rank) = apply_kill_xp(0, 0, 10_000, &c, 10);
        assert_eq!(rank, 9, "остановился на ранге 9 — дальше гейт 10/12");
        assert!(xp >= xp_to_next(9, &c), "XP накоплен в ранге 9");
        assert!(xp < xp_to_next(9, &c) + 10_000);

        // С атрибутом 12 гейт 10/12 пройден: 10000 XP хватает до ранга 11
        // (Σ(0..9)=5647, дальше need растёт быстрее остатка).
        let (_, rank) = apply_kill_xp(0, 0, 10_000, &c, 12);
        assert_eq!(rank, 11, "гейт 10/12 пройден, рост до 11");

        // Потолок 30: XP копится, ранг не растёт.
        let (xp, rank) = apply_kill_xp(0, 30, 100_000, &c, 100);
        assert_eq!(rank, 30);
        assert!(xp >= 100_000, "XP на потолке копится");
    }

    #[test]
    fn kill_xp_amount_from_difficulty() {
        // skills.md §2.3: xp = base + difficulty × per
        let base = 8 + 3 * 6;
        assert_eq!(base, 26, "пример: difficulty 3 → 26 XP");
    }

    fn upgrade_cfg() -> crate::config::UpgradeSection {
        crate::config::UpgradeSection {
            max_level: 9,
            preview: true,
            shows_chance: true,
            gold_base: 100,
            gold_per_level: 50,
            chance_base: 0.9,
            chance_per_level: 0.05,
            duration_s: 3.0,
        }
    }

    #[test]
    fn upgrade_cost_and_chance_follow_config() {
        let c = upgrade_cfg();
        assert_eq!(upgrade_cost(0, &c), 100, "+0 → +1 стоит gold_base");
        assert_eq!(upgrade_cost(3, &c), 250, "100 + 50×3");
        assert!((upgrade_chance(0, &c) - 0.9).abs() < f64::EPSILON);
        assert!((upgrade_chance(4, &c) - 0.7).abs() < 1e-9, "0.9 − 0.05×4");
        // Шанс не уходит ниже нуля на высоких уровнях.
        assert_eq!(upgrade_chance(100, &c), 0.0);
        assert_eq!(upgrade_chance(-5, &c).min(1.0), 1.0, "кламп сверху");
    }

    /// M18/F54: бонус камня к шансу — качество в процентных пунктах
    /// с затуханием 1/(1+level), сверху рейты сервера, кламп 0..=1.
    #[test]
    fn upgrade_chance_with_stone_applies_quality_and_rates() {
        let base = 0.9;
        // Разброс ±5% на нулевом уровне (малый/большой камень).
        assert!((upgrade_chance_with_stone(base, -5, 0, 1.0) - 0.85).abs() < 1e-9);
        assert!((upgrade_chance_with_stone(base, 0, 0, 1.0) - 0.9).abs() < 1e-9);
        assert!((upgrade_chance_with_stone(base, 5, 0, 1.0) - 0.95).abs() < 1e-9);
        // Динамика: чем выше уровень — тем слабее бонус (1/(1+level)).
        let hi = upgrade_chance_with_stone(base, 5, 9, 1.0);
        assert!(
            (hi - (base + 0.05 / 10.0)).abs() < 1e-9,
            "уровень 9 → +0.5%"
        );
        assert!(
            hi < upgrade_chance_with_stone(base, 5, 0, 1.0),
            "бонус затухает"
        );
        // Рейты сервера: 0 — шанс гарантированно падает; 1 — без изменений.
        assert_eq!(upgrade_chance_with_stone(base, 5, 0, 0.0), 0.0);
        assert!((upgrade_chance_with_stone(base, 5, 0, 1.0) - 0.95).abs() < 1e-9);
        // Кламп сверху: большая база × рейты не уходит выше 1.
        assert_eq!(upgrade_chance_with_stone(1.0, 5, 0, 10.0), 1.0);
        assert_eq!(
            upgrade_chance_with_stone(0.0, -5, 0, 1.0),
            0.0,
            "кламп снизу"
        );
    }

    #[test]
    fn upgrade_roll_in_unit_interval() {
        for t in [0, 1, 999, 123_456_789, i64::MAX / 2] {
            let roll = upgrade_roll(t);
            assert!((0.0..1.0).contains(&roll), "roll {roll} вне [0,1)");
        }
        // Границы шанса: 0 — всегда провал, 1 — всегда успех.
        assert!(upgrade_roll(42) >= 0.0);
        assert!(upgrade_roll(42) < 1.0);
    }

    #[test]
    fn crime_blend_partial_sum_matches_protocol_example() {
        // §10: k=0.5, T_MAX=604800, murderer=21600, marauder=7200.
        // Формула — источник истины (числовой пример документа округлён:
        // 0.5·7200·(1−21600/604800) = 3471.4 → T_new ≈ 25071).
        let (total, delta) = crime_blend(21600, 7200, 604800, 0.5);
        assert_eq!(delta, 3471, "вклад ≈ 3471 с");
        assert_eq!(total, 21600 + 3471);
        // Первое преступление — полный срок (пример §10: 21600 первым).
        let (total, delta) = crime_blend(0, 7200, 604800, 0.5);
        assert_eq!((total, delta), (7200, 7200), "T=0 → полный срок");
        // При k=1.0 — полный вклад, но множитель (1−T/T_MAX) действует:
        // 7200·(1−21600/604800) = 6943.
        let (_total, delta) = crime_blend(21600, 7200, 604800, 1.0);
        assert_eq!(delta, 6943, "k=1: вклад без «скидки» k");
        // При T → T_MAX вклад → 0.
        let (total, delta) = crime_blend(604800, 7200, 604800, 0.5);
        assert_eq!(delta, 0, "на потолке вклада нет");
        assert_eq!(total, 604800, "потолок не превышается");
        // Почти потолок: вклад мал (формула), потолок не превышен.
        let (total, _delta) = crime_blend(604700, 7200, 604800, 1.0);
        assert!((604700..=604800).contains(&total), "в пределах потолка");
        // Огромный срок при k=1: дельта обрезается T_MAX.
        let (total, delta) = crime_blend(0, 5_000_000, 604800, 1.0);
        assert_eq!(total, 604800, "потолок");
        assert_eq!(delta, 604800, "дельта ограничена потолком");
        // Отрицательный/нулевой срок — не ломает.
        let (total, delta) = crime_blend(-5, 0, 604800, 0.5);
        assert_eq!((total, delta), (0, 0));
    }

    #[test]
    fn upgrade_fail_rule_parses_capability() {
        assert_eq!(
            UpgradeFailRule::parse("break_on_fail"),
            UpgradeFailRule::Break
        );
        assert_eq!(
            UpgradeFailRule::parse("level_down"),
            UpgradeFailRule::LevelDown
        );
        // Неизвестное/отсутствующее — безопасный вариант без потери.
        assert_eq!(UpgradeFailRule::parse("whatever"), UpgradeFailRule::Keep);
        assert_eq!(UpgradeFailRule::parse(""), UpgradeFailRule::Keep);
    }

    /// M17/F30e: ресурсы по профилям — максимум, названия, шаги
    /// (ярость падает вне боя, энергия растёт всегда, мана как раньше).
    #[test]
    fn resource_by_profile_step_and_names() {
        let example =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("server.example.toml");
        let cfg = crate::config::Config::load(&example).expect("example валиден");
        let res = &cfg.gameplay.resource;

        assert_eq!(resource_max("warrior", res, 60), 100, "ярость");
        assert_eq!(resource_max("archer", res, 60), 100, "энергия");
        assert_eq!(resource_max("mage", res, 60), 60, "мана — колонка");
        assert_eq!(resource_name("warrior"), "ярости");
        assert_eq!(resource_name("archer"), "энергии");
        assert_eq!(resource_name("mage"), "маны");

        // Ярость: −5/сек вне боя, в бою не трогаем, пол — 0.
        assert_eq!(resource_step("warrior", 50, 100, false, &cfg), Some(45));
        assert_eq!(resource_step("warrior", 50, 100, true, &cfg), None);
        assert_eq!(resource_step("warrior", 3, 100, false, &cfg), Some(0));
        // Энергия: +20/сек всегда, потолок 100, полная — молчим.
        assert_eq!(resource_step("archer", 80, 100, true, &cfg), Some(100));
        assert_eq!(resource_step("archer", 95, 100, false, &cfg), Some(100));
        assert_eq!(resource_step("archer", 100, 100, true, &cfg), None);
        // Мана: прежняя модель (вне боя 60/45 → +2, в бою 1% → +1).
        assert_eq!(resource_step("mage", 45, 60, false, &cfg), Some(47));
        assert_eq!(resource_step("mage", 45, 60, true, &cfg), Some(46));
        assert_eq!(resource_step("mage", 60, 60, false, &cfg), None);
        // Ярость за урон: 15 %, вверх; не-воин — 0.
        assert_eq!(rage_gain(res, "warrior", 10), 2, "1.5 → 2");
        assert_eq!(rage_gain(res, "warrior", 1), 1, "0.15 → 1");
        assert_eq!(rage_gain(res, "mage", 10), 0);
        assert_eq!(rage_gain(res, "warrior", 0), 0);
    }
}

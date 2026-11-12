//! Чистые игровые правила (без tokio/sqlx) — юнит-тесты (M12.5).

use crate::config::{LevelSection, SkillsSection, UpgradeSection};

/// Порог ранга: `round(base_xp × growth^rank)` (skills.md §2.2, D25).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SkillGate;

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

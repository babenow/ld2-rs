//! Чистые игровые правила (без tokio/sqlx) — юнит-тесты (M12.5).

use crate::config::{SkillsSection, UpgradeSection};

/// Порог ранга: `round(base_xp × growth^rank)` (skills.md §2.2, D25).
pub fn xp_to_next(rank: u32, cfg: &SkillsSection) -> i64 {
    ((cfg.base_xp as f64) * cfg.growth.powi(rank as i32)).round() as i64
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
}

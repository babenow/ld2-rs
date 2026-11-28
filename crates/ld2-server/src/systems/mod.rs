//! Чистые игровые правила (без tokio/sqlx) — юнит-тесты (M12.5).

use crate::config::{LevelSection, SkillsSection, UpgradeSection};

mod combat;
mod runes;
mod upgrade;
mod xp;

pub use combat::*;
pub use runes::*;
pub use upgrade::*;
pub use xp::*;

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
            ranks_per_level: 2,
            gates: vec![
                SkillGate { rank: 10, attr: 12 },
                SkillGate { rank: 20, attr: 18 },
                SkillGate { rank: 30, attr: 25 },
            ],
            kill_xp_base: 4,
            kill_xp_per_difficulty: 3,
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
            rank_xp_mult: vec![1.0, 1.2, 1.45, 1.75, 2.1],
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

    /// M21/M39: формула уровня `round(40 × 1.02^L × mult(ранг))` —
    /// монотонно растёт; на рангах 2+ действует множитель.
    #[test]
    fn profession_level_curve_grows() {
        let cfg = prof_cfg();
        assert_eq!(profession_level_xp(&cfg, 0), 40, "L0, ранг1");
        assert_eq!(profession_level_xp(&cfg, 1), 41, "L1, ранг1");
        assert_eq!(profession_level_xp(&cfg, 50), 129, "L50, ранг2 ×1.2");
        assert_eq!(profession_level_xp(&cfg, 100), 420, "L100, ранг3 ×1.45");
        let (a, b) = (
            profession_level_xp(&cfg, 150),
            profession_level_xp(&cfg, 151),
        );
        assert!(b > a, "каждый уровень дороже: {a} → {b}");
        let top = profession_level_xp(&cfg, 249);
        assert!(
            (11_000..=12_000).contains(&top),
            "потолочный уровень (ранг5 ×2.1): {top}"
        );
    }

    /// M39: число успешных действий на ранг растёт мягкой геометрией —
    /// иначе смысл множителей теряется. Считаем по XP действия ранга
    /// рецепта (40 + 25×ранг); точный XP зависит от источника.
    #[test]
    fn profession_rank_action_counts_grow_softly() {
        let cfg = prof_cfg();
        let actions: Vec<f64> = (1..=crate::systems::profession_rank_max(&cfg))
            .map(|rank| {
                let start = (rank - 1) * cfg.rank_span;
                let total: i64 = (start..start + cfg.rank_span)
                    .map(|level| profession_level_xp(&cfg, level))
                    .sum();
                let per_action =
                    cfg.xp_per_craft_base + cfg.xp_per_craft_per_rank * i64::from(rank);
                total as f64 / per_action.max(1) as f64
            })
            .collect();
        assert!(
            actions.windows(2).all(|w| w[1] > w[0]),
            "действий на ранг больше: {actions:?}"
        );
        for w in actions.windows(2) {
            let ratio = w[1] / w[0];
            assert!(
                (1.5..=3.5).contains(&ratio),
                "мягкая геометрия, а не скачок: {actions:?}"
            );
        }
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
        // Потолок: на L250 опыт не копится (порог L249 — 11 634).
        assert_eq!(profession_level_up(&cfg, 40, 249, 20_000), (0, 250));
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
        let cap = c.rank_max;
        let (xp, rank) = apply_kill_xp(0, 0, 10_000, &c, 10, cap);
        assert_eq!(rank, 9, "остановился на ранге 9 — дальше гейт 10/12");
        assert!(xp >= xp_to_next(9, &c), "XP накоплен в ранге 9");
        assert!(xp < xp_to_next(9, &c) + 10_000);

        // С атрибутом 12 гейт 10/12 пройден: 10000 XP хватает до ранга 11
        // (Σ(0..9)=5647, дальше need растёт быстрее остатка).
        let (_, rank) = apply_kill_xp(0, 0, 10_000, &c, 12, cap);
        assert_eq!(rank, 11, "гейт 10/12 пройден, рост до 11");

        // Потолок 30: XP копится, ранг не растёт.
        let (xp, rank) = apply_kill_xp(0, 30, 100_000, &c, 100, cap);
        assert_eq!(rank, 30);
        assert!(xp >= 100_000, "XP на потолке копится");
    }

    /// M34: множители профессии перемножаются (премиум × событие).
    #[test]
    fn profession_multipliers_stack() {
        assert_eq!(profession_xp_multiplier(false, 1.5, &[]), 1.0);
        assert_eq!(profession_xp_multiplier(true, 1.5, &[]), 1.5);
        assert!(
            (profession_xp_multiplier(true, 1.5, &[1.2]) - 1.8).abs() < 1e-9,
            "премиум × акция"
        );
        assert_eq!(
            profession_xp_multiplier(false, 1.5, &[1.2, 0.5]),
            0.6,
            "множители перемножаются (включая дебафф)"
        );
        assert_eq!(
            profession_xp_multiplier(false, 1.5, &[1.2, -0.5]),
            1.2,
            "неположительный отброшен"
        );
        assert_eq!(
            profession_xp_multiplier(true, 0.5, &[]),
            1.0,
            "премиум < 1 не режет"
        );
    }

    /// M33: премиум-множитель опыта — +50 % с округлением.
    #[test]
    fn premium_gain_applies_multiplier() {
        assert_eq!(premium_gain(13, 1.5), 20, "19.5 → 20");
        assert_eq!(premium_gain(10, 1.5), 15);
        assert_eq!(premium_gain(7, 2.0), 14);
        assert_eq!(premium_gain(0, 1.5), 0);
        assert_eq!(premium_gain(5, 0.5), 5, "множитель < 1 не режет");
    }

    #[test]
    fn kill_xp_amount_from_difficulty() {
        // skills.md §2.3: xp = base + difficulty × per (M31: 4 + 3×3)
        let base = 4 + 3 * 3;
        assert_eq!(base, 13, "пример: difficulty 3 → 13 XP");
    }

    /// M31: боевые/защитные/магические навыки — не более 2 рангов за
    /// уровень персонажа; остальные группы не ограничены уровнем.
    #[test]
    fn skill_rank_cap_follows_level_for_combat_groups() {
        let c = cfg();
        assert_eq!(skill_rank_cap(&c, "weapon", 1), 2, "уровень 1 → 2 ранга");
        assert_eq!(skill_rank_cap(&c, "defense", 5), 10);
        assert_eq!(skill_rank_cap(&c, "magic", 10), 20);
        assert_eq!(skill_rank_cap(&c, "weapon", 60), 30, "не выше rank_max");
        assert_eq!(
            skill_rank_cap(&c, "stealth", 1),
            c.rank_max,
            "воровские уровнем не ограничены"
        );
        assert_eq!(skill_rank_cap(&c, "practice", 1), c.rank_max);
    }

    /// M31: XP сверх потолка по уровню копится, ранг не растёт — при
    /// следующем уровне рост продолжится со накопленным опытом.
    #[test]
    fn level_cap_holds_rank_but_keeps_xp() {
        let c = cfg();
        let cap = skill_rank_cap(&c, "weapon", 1); // 2
        let (xp, rank) = apply_kill_xp(0, 0, 10_000, &c, 100, cap);
        assert_eq!(rank, 2);
        assert!(xp >= xp_to_next(2, &c), "XP копится на потолке");
        // Уровень вырос → тот же XP добирает ранги дальше.
        let (_, rank) = apply_kill_xp(xp, rank, 0, &c, 100, skill_rank_cap(&c, "weapon", 10));
        assert!(rank > 2, "после роста уровня ранг растёт: {rank}");
    }

    /// M32: точность/уклон → шанс попадания (кламп 5..95).
    #[test]
    fn hit_chance_subtracts_dodge_and_clamps() {
        assert_eq!(hit_chance(80, 6), 74);
        assert_eq!(hit_chance(80, 0), 80);
        assert_eq!(hit_chance(20, 60), 5, "пол всегда 5 %");
        assert_eq!(hit_chance(99, 0), 95, "потолок 95 %");
        assert!(hit_rolls(80, 6, 73.9));
        assert!(!hit_rolls(80, 6, 74.0));
    }

    /// M32: бросок урона в диапазоне, границы и детерминизм.
    #[test]
    fn roll_damage_covers_range() {
        assert_eq!(roll_damage(11, 11, 0.42), 11, "min == max");
        assert_eq!(roll_damage(59, 75, 0.0), 59);
        assert_eq!(roll_damage(59, 75, 0.999), 75);
        let mid = roll_damage(59, 75, 0.5);
        assert!((59..=75).contains(&mid), "{mid}");
    }

    /// M32: парирование, броня и магрезист — снижение урона, минимум 1.
    #[test]
    fn mitigation_reduces_damage_but_keeps_min_one() {
        assert_eq!(after_parry(11), 6, "половина вверх");
        assert_eq!(after_parry(10), 5);
        assert_eq!(after_parry(1), 1);
        assert_eq!(after_armor(11, 6), 5);
        assert_eq!(after_armor(3, 10), 1, "минимум 1");
        assert_eq!(after_resist(10, 6), 9, "6 % от 10");
        assert_eq!(after_resist(50, 50), 25);
        assert_eq!(after_resist(1, 90), 1, "минимум 1");
    }

    /// M32: урон способности — промах/парирование/броня и множитель.
    #[test]
    fn ability_damage_rolls_miss_parry_armor() {
        // 11 ×1.8 = 20 → броня 6 → 14.
        assert_eq!(
            ability_damage(80, 6, 11, 11, 6, 3, 1.8, 10.0, 0.5, 50.0),
            Some(14)
        );
        // Промах: бросок 90 против шанса 74.
        assert_eq!(
            ability_damage(80, 6, 11, 11, 6, 3, 1.8, 90.0, 0.5, 50.0),
            None
        );
        // Парирование: 20 → 10 → −6 брони = 4.
        assert_eq!(
            ability_damage(80, 6, 11, 11, 6, 3, 1.8, 10.0, 0.5, 1.0),
            Some(4)
        );
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
        assert_eq!(resource_step("warrior", 50, 100, false, 0, &cfg), Some(45));
        assert_eq!(resource_step("warrior", 50, 100, true, 0, &cfg), None);
        assert_eq!(resource_step("warrior", 3, 100, false, 0, &cfg), Some(0));
        // Энергия: +20/сек всегда, потолок 100, полная — молчим.
        assert_eq!(resource_step("archer", 80, 100, true, 0, &cfg), Some(100));
        assert_eq!(resource_step("archer", 95, 100, false, 0, &cfg), Some(100));
        assert_eq!(resource_step("archer", 100, 100, true, 0, &cfg), None);
        // Мана: прежняя модель (вне боя 60/45 → +2, в бою 1% → +1).
        assert_eq!(resource_step("mage", 45, 60, false, 0, &cfg), Some(47));
        assert_eq!(resource_step("mage", 45, 60, true, 0, &cfg), Some(46));
        assert_eq!(resource_step("mage", 60, 60, false, 0, &cfg), None);
        // M30: духовность 10 при spirit_regen_pct=1.0 → ×1.1,
        // округление вверх: вне боя 2→3, в бою 1→2; ярость не зависит.
        assert_eq!(
            resource_step("mage", 45, 60, false, 10, &cfg),
            Some(48),
            "духовность ускоряет реген маны"
        );
        assert_eq!(
            resource_step("mage", 45, 60, true, 10, &cfg),
            Some(47),
            "духовность ускоряет реген маны в бою"
        );
        assert_eq!(
            resource_step("warrior", 50, 100, false, 10, &cfg),
            Some(45),
            "ярость от духовности не зависит"
        );
        // Ярость за урон: 15 %, вверх; не-воин — 0.
        assert_eq!(rage_gain(res, "warrior", 10), 2, "1.5 → 2");
        assert_eq!(rage_gain(res, "warrior", 1), 1, "0.15 → 1");
        assert_eq!(rage_gain(res, "mage", 10), 0);
        assert_eq!(rage_gain(res, "warrior", 0), 0);
    }

    /// M30: регенерация здоровья (character-params.md §3.1) —
    /// дефолты `HealthSection`: вне боя 100/60 → +2, в бою 0.5 % → +1,
    /// мёртвые (cur=0) и полный бар не регенерируют.
    #[test]
    fn health_step_regens_out_and_in_combat() {
        let example =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("server.example.toml");
        let cfg = crate::config::Config::load(&example).expect("example валиден");
        assert_eq!(health_step(50, 100, false, &cfg), Some(52), "вне боя");
        assert_eq!(health_step(50, 100, true, &cfg), Some(51), "в бою");
        assert_eq!(health_step(99, 100, false, &cfg), Some(100), "кламп");
        assert_eq!(health_step(100, 100, false, &cfg), None, "полный");
        assert_eq!(health_step(0, 100, false, &cfg), None, "мёртвый");
    }

    /// M25: пол минимальной скорости атаки (настройка сервера) и
    /// ускорение фазы босса.
    #[test]
    fn mob_attack_interval_respects_floor() {
        assert_eq!(mob_attack_interval(1000, 1.0, 500), 1000, "обычный");
        assert_eq!(
            mob_attack_interval(100, 1.0, 500),
            500,
            "конфиг ниже настройки"
        );
        assert_eq!(mob_attack_interval(1000, 0.7, 500), 700, "фаза ускоряет");
        assert_eq!(mob_attack_interval(500, 0.1, 500), 500, "пол держится");
    }
}

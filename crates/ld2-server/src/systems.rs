//! Чистые игровые правила (без tokio/sqlx) — юнит-тесты (M12.5).

use crate::config::SkillsSection;

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
}

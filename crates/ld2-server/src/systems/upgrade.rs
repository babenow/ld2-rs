//! Правила улучшения предметов (ФР-10).

use super::*;

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

/// Детерминированный бросок улучшения (`roll ∈ [0,1)`) — только для
/// юнит-тестов; продакшн — `world::roll_chance` (CSPRNG, R7).
pub fn upgrade_roll(now_ms: i64) -> f64 {
    // Взбитое время — без rand-зависимости (как в combat.flee).
    let mixed = now_ms.wrapping_mul(0x9E37_79B9_7F4A_7C15_u64 as i64);
    (mixed.unsigned_abs() % 10_000) as f64 / 10_000.0
}

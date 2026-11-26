//! Секции `[world]`, `[skills]`, `[gameplay.combat]`, `[gameplay.level]`.

use super::*;

/// M20: диапазон респавна в секундах — два числа `{min_s, max_s}`,
/// факт возрождения случайный в диапазоне (план M20, требование 1).
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct RespawnRange {
    pub min_s: i64,
    pub max_s: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldSection {
    /// Каталог статического контента (JSON) → сид в PostgreSQL (D26).
    pub content_dir: PathBuf,
    /// Частота тик-лупа мира, Гц (D31), `1..=100`.
    pub tick_hz: u32,
    /// Периодический флаш онлайн-состояния, с (D27).
    pub autosave_interval_s: u64,
    /// M20: время жизни трупа, с (вместе с неразобранным лутом);
    /// `0` — трупы не удаляются. Для теста —60.
    #[serde(default = "default_corpse_ttl")]
    pub corpse_ttl_s: i64,
    /// M20: время жизни выброшенного предмета, с; `0` — не удаляется.
    /// Для теста —60.
    #[serde(default = "default_drop_ttl")]
    pub drop_ttl_s: i64,
    /// M25: время жизни строки журнала (Лог), с; `0` — не удаляются.
    /// Запрашивается клиентом через session.start; по умолчанию 30.
    #[serde(default = "default_journal_ttl")]
    pub journal_ttl_s: i64,
    /// M25: локация респавна после смерти; пусто — спавн-локация
    /// контента (`meta.spawn_location`).
    #[serde(default)]
    pub respawn_location_id: Option<String>,
    /// M20: дефолтный респавн мобов `{min_s, max_s}` — для мобов без
    /// `respawn` в контенте; нет поля и нет дефолта — моб не
    /// возрождается (план M20, решение 5).
    #[serde(default)]
    pub respawn_default: Option<RespawnRange>,
    /// M27 (hot reload, фаза 1): период проверки каталога контента, с;
    /// `0` — только по SIGHUP/admin-команде. Изменение отпечатка
    /// (`size`+`mtime` файлов каталога) → перечитать контент и применить
    /// без рестарта.
    #[serde(default = "default_content_poll_s")]
    pub content_poll_s: u64,
}

const fn default_corpse_ttl() -> i64 {
    60
}

const fn default_journal_ttl() -> i64 {
    30
}

const fn default_drop_ttl() -> i64 {
    60
}

const fn default_content_poll_s() -> u64 {
    30
}

/// M31: по умолчанию — 2 ранга за уровень персонажа.
const fn default_ranks_per_level() -> u32 {
    2
}

/// Константы модели опыта навыков (skills.md §8 — «конфиг сервера», D25).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsSection {
    /// `xp_to_next(rank) = round(base_xp × growth^rank)`.
    pub base_xp: i64,
    pub growth: f64,
    /// Потолок ранга (skills.md §2.2) → `E_RANK_CAP`.
    pub rank_max: u32,
    /// Гейты по атрибутам: ранг → требуемое значение (→ `E_REQUIREMENT_ATTR`).
    pub gates: Vec<SkillGate>,
    /// M31: потолок ранга боевых/защитных/магических навыков — не
    /// более N рангов за уровень персонажа (2 → ранг ≤ 2×уровень).
    /// Остальные группы (stealth/practice/social) не ограничены.
    #[serde(default = "default_ranks_per_level")]
    pub ranks_per_level: u32,
    /// Опыт за убийство (skills.md §2.3): `(base + difficulty × per)`.
    pub kill_xp_base: i64,
    pub kill_xp_per_difficulty: i64,
    /// Активные способности `cmd/skill.use` (skills.md §4, M14):
    /// id → параметры (кулдаун, множитель урона, гейт по навыку).
    #[serde(default)]
    pub active: std::collections::HashMap<String, ActiveSkill>,
}

/// Параметры активной способности (§4: открывается навыком, кулдаун,
/// эффект) — всё в конфиге (D25).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveSkill {
    /// Кулдаун, с (серверный, E_COOLDOWN + retry_after_s).
    pub cooldown_s: f64,
    /// Множитель урона от `gameplay.combat.base_damage` (§4: ×1.8).
    pub damage_mult: f64,
    /// Открывается рангом этого навыка (`E_REQUIREMENT_ATTR`); пусто — без гейта.
    #[serde(default)]
    pub req_skill: Option<String>,
    /// Требуемый ранг `req_skill`.
    #[serde(default)]
    pub req_rank: u32,
    /// M17/F30e: стоимость в ресурсе класса — ярость/энергия/мана
    /// (0 — бесплатно, дефолт старых конфигов).
    #[serde(default)]
    pub cost: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillGate {
    pub rank: u32,
    pub attr: i64,
}

/// Параметры боя (M12.5, D25 — всё в конфиге).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CombatSection {
    /// Период атак в бою, с (тик-луп `combat.attack`).
    pub attack_interval_s: f64,
    /// M25: минимальная скорость атаки моба, с — пол интервала
    /// (настройка сервера); контент-параметр моба `attack_speed`
    /// (мс) прибавляется сверху — замедление.
    #[serde(default = "default_min_attack_interval_s")]
    pub min_attack_interval_s: f64,
    /// Базовый урон персонажа (кулак; оружие/навыки добавляет
    /// `stats::derive` — M32: бой берёт реальный урон листа).
    pub base_damage: i64,
    /// M32: зерно ГПСЧ боевых бросков (точность/парирование); `None`/0 —
    /// случайное на старте; тесты задают фиксированное для детерминизма.
    #[serde(default)]
    pub rng_seed: Option<u64>,
    /// Шанс успешного побега (`cmd/combat.flee`), `0..=1`.
    pub flee_chance: f64,
}

/// R15: дефолт per-session лимита мутирующих команд (в минуту).
pub(super) const fn default_cmd_rate_per_min() -> u32 {
    240
}

const fn default_min_attack_interval_s() -> f64 {
    0.5
}

/// M15/F24: уровень персонажа — квадратичная кривая опыта за
/// убийства: `XP = round(A×level² + B×level + C)` — опыт перехода
/// `level → level+1`. Роли (уточнение пользователя 2026-11-09):
/// A — рост на высоких уровнях, B — темп в начале, C — смещение.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelSection {
    /// Коэффициент A квадрата (≥ 0; дефолт 50).
    pub a: f64,
    /// Коэффициент B линейной слагаемой (≥ 0; дефолт 50).
    pub b: f64,
    /// Коэффициент C — базовое смещение (≥ 0; дефолт 0).
    pub c: f64,
    /// Потолок: опыт копится без роста уровня (зерно навыков).
    pub max_level: u32,
}

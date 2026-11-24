//! Секции `[gameplay]`: правила, квесты, магазин, крим, чат, инвентарь, улучшение, магия, мана, ресурсы, профессии.

use super::world::default_cmd_rate_per_min;
use super::*;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameplaySection {
    /// Лимит слотов персонажей → §5 `char_slot_limit` (DoD M2: конфиг, не код).
    pub char_slot_limit: u32,
    /// R15: per-session лимит мутирующих команд в минуту (§9
    /// `E_RATE_LIMITED`); `0` — выключен. Атака минимум раз в 0.5 с →
    /// дефолт 240 покрывает бой с запасом.
    #[serde(default = "default_cmd_rate_per_min")]
    pub cmd_rate_per_min: u32,
    pub profession_limit: u32,
    /// Бой (M12.5): тик-луп атак и правила побега.
    pub combat: CombatSection,
    /// Capability-флаги §5: ключи с точками — **в кавычках**:
    /// `"loot.roll" = false`. Неизвестные ключи разрешены (фичи открыты).
    pub features: HashMap<String, FeatureValue>,
    /// Модель преступности §10 — параметры только отсюда.
    pub crime: CrimeSection,
    pub chat: ChatSection,
    pub inventory: InventorySection,
    pub upgrade: UpgradeSection,
    pub magic: MagicSection,
    /// Профессии и крафт (skills.md §8, M12.6).
    pub professions: ProfessionsSection,
    /// M15/F24: уровень персонажа (окно персонажа).
    pub level: LevelSection,
    /// Максимум участников группы (ФР-14, M12.7b).
    pub party_max_members: u32,
    /// M17/F30e: ресурсы по классам (WoW); секция опциональна —
    /// без неё действуют дефолты (обратная совместимость конфигов).
    #[serde(default)]
    pub resource: ResourceSection,
    /// M18/F36: квесты — лимит текущих (взятых) квестов на персонажа
    /// → §5 `quests.max_active`; секция опциональна (дефолт 10).
    #[serde(default)]
    pub quests: QuestsSection,
    /// M18/F39: магазины NPC (наценки/скидки); опциональна.
    #[serde(default)]
    pub shop: ShopSection,
    /// M18/F54: рейты сервера — коэффициент любых шансов; опциональна.
    #[serde(default)]
    pub rates: RatesSection,
}

/// Лимиты квестов (M18/F36): `max_active` — взятых квестов (фазы
/// active/turn_in) одновременно; сверх лимита `quest.accept` →
/// `E_QUEST_LIMIT`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestsSection {
    pub max_active: u32,
}

impl Default for QuestsSection {
    fn default() -> Self {
        Self { max_active: 10 }
    }
}

/// M18/F39: магазины NPC — цены считаются сервером (контент задаёт
/// базу `gold_value` и наценку торговца): `server_markup` — множитель
/// наценки сервера; `max_discount_bp` — потолок суммарной скидки
/// (аккаунт + навыки); `skill_discounts` — навык → множитель скидки
/// **за ранг** (haggle 0.02 = −2%/ранг). Секция опциональна.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShopSection {
    pub server_markup: f64,
    pub max_discount_bp: i64,
    #[serde(default)]
    pub skill_discounts: HashMap<String, f64>,
}

impl Default for ShopSection {
    fn default() -> Self {
        let mut skill_discounts = HashMap::new();
        skill_discounts.insert("haggle".into(), 0.02);
        Self {
            server_markup: 1.0,
            max_discount_bp: 5000,
            skill_discounts,
        }
    }
}

/// **Рейты сервера** (M18/F54): единый коэффициент шансов — `chance`
/// умножает любой бросок вероятности (заточка и др.) перед клампом в
/// `0..=1`. `1.0` — дефолт (шансы как в формуле конфига), `0.0` — все
/// шансы гарантированно проваливаются. Секция опциональна.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RatesSection {
    /// Множитель шансов (`0.0..=10.0`).
    pub chance: f64,
}

impl Default for RatesSection {
    fn default() -> Self {
        Self { chance: 1.0 }
    }
}

/// Значение capability-флага §5: булево или строка (`upgrade.rules`).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FeatureValue {
    Bool(bool),
    Str(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrimeSection {
    /// Потолок суммарного срока статуса, с.
    pub t_max_s: u64,
    /// Коэффициент «частичного сложения» §10, `(0; 1]`.
    pub blend_k: f64,
    /// Базовые сроки по видам: `murderer_s`, `marauder_s`, …
    pub base: HashMap<String, u64>,
    pub kinds: Vec<CrimeKind>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrimeKind {
    pub kind: String,
    pub title: String,
    pub severity: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatSection {
    /// Размер кольцевой истории в БД (0 — не хранить).
    pub server_history: u32,
    pub max_len: u32,
    pub cooldown_s: f64,
    pub channels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventorySection {
    pub slots: u32,
    pub equip_slots: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeSection {
    pub max_level: u32,
    pub preview: bool,
    pub shows_chance: bool,
    /// Базовая стоимость улучшения в золоте (ФР-10, M12.5c).
    pub gold_base: i64,
    /// Прибавка к стоимости за каждый уровень.
    pub gold_per_level: i64,
    /// Шанс успеха с уровня +0 (`0..=1`).
    pub chance_base: f64,
    /// Снижение шанса за уровень.
    pub chance_per_level: f64,
    /// Длительность анимации улучшения, сек (M18/F53). Прогресс-бар
    /// клиента идёт это время; результат приходит после.
    pub duration_s: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicSection {
    /// Режим восстановления зарядов рун (ADR-002, оживлён M18/F58c
    /// по ADR-012 §4): `time` — тикер по `rune_recharge_s`;
    /// `item` — прежнее поведение (заряды возвращаются только при
    /// извлечении руны). Ранее ключ был декоративным.
    pub rune_recharge: String,
    /// M18/F58c (ADR-012 §4): секунд на восстановление **одного**
    /// заряда руны — по аналогии с `resource.energy_regen_s`
    /// (тик-луп, раз в секунду, только онлайн-персонажи).
    pub rune_recharge_s: f64,
    pub scroll_requires_target: bool,
    pub mana: ManaSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManaSection {
    /// Реген вне боя, с на 1%.
    pub regen_out_s: u64,
    /// Реген в бою, %/с.
    pub regen_in_pct_s: f64,
    /// Стоимость каста руны в мане.
    pub rune_cost: u32,
}

/// M17/F30e: ресурсы по классам как в WoW (`[gameplay.resource]`) —
/// воин: ярость (копится за урон, падает вне боя), лучник: энергия
/// (тратится активными навыками, восстанавливается всегда), маг:
/// мана (`[gameplay.magic.mana]`, без изменений).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSection {
    /// Максимум ярости (воин).
    pub rage_max: u32,
    /// Максимум энергии (лучник).
    pub energy_max: u32,
    /// Доля урона (нанесённого и полученного) → ярость.
    pub rage_gain_pct: f64,
    /// Падение ярости вне боя, ед/сек.
    pub rage_decay_s: f64,
    /// Восстановление энергии, ед/сек (в бою и вне).
    pub energy_regen_s: f64,
}

impl Default for ResourceSection {
    fn default() -> Self {
        Self {
            rage_max: 100,
            energy_max: 100,
            rage_gain_pct: 0.15,
            rage_decay_s: 5.0,
            energy_regen_s: 20.0,
        }
    }
}

/// Баланс профессий (skills.md §8 `[professions]`, D25 — из конфига).
/// M21: две шкалы — **уровень** (0…`max_level`, XP-порог растёт) и
/// **ранг** (1…`ceil(max_level/rank_span)`, границы кратно `rank_span`;
/// повышение — у тренера за золото или наградой квеста).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfessionsSection {
    /// XP за успешный крафт: `base + per_rank × ранг_рецепта`.
    pub xp_per_craft_base: i64,
    pub xp_per_craft_per_rank: i64,
    /// Доля XP при неудаче (skills.md §5.1).
    pub fail_xp_ratio: f64,
    /// Порог уровня: `round(base × growth^L)` — каждый уровень дороже
    /// (согласовано: `40 × 1.02^L`).
    pub level_xp_base: i64,
    pub level_xp_growth: f64,
    /// Потолок уровня (на нём опыт перестаёт копиться; верхний ранг
    /// «Мастер» заканчивает на этом уровне).
    pub max_level: u32,
    /// Уровней в ранге — границы рангов кратно этому (50).
    pub rank_span: u32,
    /// Цена повышения ранга у тренера: `round(base × growth^ранг)`
    /// (согласовано: `200 × 1.5^ранг`).
    pub rank_up_gold_base: i64,
    pub rank_up_gold_growth: f64,
}

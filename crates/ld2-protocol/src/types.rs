//! Схемы общих типов контракта — `docs/protocol.md` §6 + состояния снапшота.
//!
//! Правило: клиент рендерит любые значения (открытые строки `kind`/`rarity`/`severity`
//! расширимы сервером), незнакомые поля в структурах с `#[serde(flatten)] extra`
//! сохраняются и не ломают десериализацию (§2.3, §11).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Открытый JSON-объект (статы предмета, патчи, «затраты» и т.п.).
pub type JsonMap = serde_json::Map<String, serde_json::Value>;

// ─── §6: базовые типы ────────────────────────────────────────────────────────

/// Здоровье. `Mp` — ресурс заклинаний (D4, ADR-002): руна тратит ману + заряд,
/// свиток — без маны.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bar {
    pub cur: i64,
    pub max: i64,
}

/// Тип-синоним для читаемости: `hp`.
pub type Hp = Bar;
/// Тип-синоним для читаемости: `mp`.
pub type Mp = Bar;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Player,
    Monster,
    Npc,
    Corpse,
    Object,
}

/// Строка в `location.entities` (§6). `was_player` — ключевой маркер трупа
/// игрока (ФР-11: лут с него = статус «Мародёр»).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub kind: EntityKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hp: Option<Hp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Только для `kind = corpse`: труп игрока (предупреждение о мародёрстве).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was_player: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggro: Option<bool>,
}

/// Выход из локации; `dir` — опционное каноническое направление для хоткеев.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exit {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

/// Полный снапшот локации (§6, `evt/location/changed`, ack `location.*`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocationSnapshot {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub flags: Vec<String>,
    pub text: String,
    #[serde(default)]
    pub exits: Vec<Exit>,
    #[serde(default)]
    pub entities: Vec<Entity>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Требования предмета (§6): сервер присылает `null` для отсутствующих.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Requires {
    #[serde(default)]
    pub attr: BTreeMap<String, i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profession: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<i32>,
}

/// Заряды руны в сокете (D4: каст тратит ману И заряд).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuneInfo {
    pub name: String,
    pub spell: String,
    pub charges: i32,
    pub max: i32,
}

/// Сокет посоха: `rune: null` — свободный (`◈◈○` в UI, ФР-09).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Socket {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rune: Option<RuneInfo>,
}

/// Предмет инвентаря/экипировки/трупа (§6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemSnapshot {
    pub uid: String,
    /// Тип: weapon|armor|staff|potion|scroll|rune|material|quest (открытая строка).
    #[serde(rename = "type")]
    pub item_type: String,
    pub name: String,
    /// Rarity: открытая строка (common…legendary + расширения сервера).
    pub rarity: String,
    /// Уровень улучшения `+N` (ФР-10).
    pub upgrade: i32,
    #[serde(default)]
    pub stats: JsonMap,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<Requires>,
    /// Только для предметов со сокетами (посохи).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sockets: Option<Vec<Socket>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qty: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gold_value: Option<i64>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Навык персонажа (§6; `name` опционален — tz-снапшот шлёт только `id`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillSnapshot {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub rank: u32,
    pub xp: i64,
    pub xp_to_next: i64,
}

/// Уровень опасности статуса преступления (§5 `crime.kinds`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    /// Неизвестный сервером уровень — UI использует нейтральный цвет (§2.3).
    #[serde(other)]
    Unknown,
}

/// Активный статус преступления (§6). `expires_at` — epoch-секунды (§1.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrimeStatus {
    pub kind: String,
    pub severity: Severity,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestPhase {
    Available,
    Active,
    TurnIn,
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestGoal {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cur: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RewardPreview {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xp: Option<i64>,
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
}

/// Квест в журнале (§6, ФР-12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestState {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub state: QuestPhase,
    #[serde(default)]
    pub goals: Vec<QuestGoal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_preview: Option<RewardPreview>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

// ─── Состояния снапшота и социальных событий ─────────────────────────────────

/// Профессия персонажа (`cmd/craft.list`, `evt/craft/profession`, §7–§8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfessionSnapshot {
    pub id: String,
    pub rank: u32,
    pub xp: i64,
    pub xp_to_next: i64,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Блок статусов преступности в снапшоте (§4.8.2).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CrimeState {
    #[serde(default)]
    pub statuses: Vec<CrimeStatus>,
}

/// Строка журнала нарушений (`evt/crime/added.log_entry`, §8; §12.3 пример).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrimeLogEntry {
    pub kind: String,
    /// Epoch-секунды момента нарушения.
    pub at: i64,
    /// Сколько добавлено секунд (после «частичного сложения», §10).
    pub delta_s: i64,
    /// Итоговый общий срок после добавления.
    pub total_s: i64,
}

/// Экипировка: слот → `item_uid` (или `null`, если пусто) — §4.8.2.
pub type EquipMap = BTreeMap<String, Option<String>>;

/// Полное состояние персонажа в `evt/state/snapshot` (§3, §4.8.2 ТЗ).
///
/// `xp_pool` — свободные очки опыта от квестов (D3/D4: вкладываются командой
/// `char.xp_invest`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharacterState {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub attrs: BTreeMap<String, i64>,
    #[serde(default)]
    pub skills: Vec<SkillSnapshot>,
    #[serde(default)]
    pub professions: Vec<ProfessionSnapshot>,
    pub hp: Hp,
    pub mp: Mp,
    #[serde(default)]
    pub xp_pool: i64,
    #[serde(default)]
    pub equip: EquipMap,
    #[serde(default)]
    pub inventory: Vec<ItemSnapshot>,
    #[serde(default)]
    pub crime: CrimeState,
    #[serde(default)]
    pub quests: Vec<QuestState>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Сводка персонажа в списке аккаунта (§5 `characters[]`, ФР-02).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharacterSummary {
    pub id: String,
    pub name: String,
    pub location: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Активные статусы преступления (`kind`-ы) — бейджи на карточке.
    #[serde(default)]
    pub crime: Vec<String>,
    #[serde(default)]
    pub online: bool,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Участник группы (`evt/party/updated`, §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartyMember {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hp: Option<Hp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<bool>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Полное состояние группы (§8): `party: null` — группы нет.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartyState {
    pub id: String,
    pub leader: String,
    #[serde(default)]
    pub members: Vec<PartyMember>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Участник гильдии (`evt/guild/updated`, §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildMember {
    pub id: String,
    pub name: String,
    pub rank: String,
    #[serde(default)]
    pub online: bool,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Полное состояние гильдии (§8): `guild: null` — гильдии нет.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuildState {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub members: Vec<GuildMember>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

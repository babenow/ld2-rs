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
    /// M20/F20b: объект-ресурс (жила/дерево/трава) — `cmd/resource.gather`.
    Resource,
    /// M20/F20c: контейнер (сундук) — лут `loot:<entity>:<idx>`
    /// однократно на персонажа (паритет трупа).
    Container,
    /// R12 (§6): незнакомое значение kind — не роняем разбор Entity
    /// (иначе терялся весь снапшот); рендер — как нейтральное.
    #[serde(other)]
    Unknown,
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
    /// M25: босс — текущая фаза (имя/глиф/каст) для списка монстров.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boss: Option<BossState>,
    /// M25: кого монстр атакует (id цели) — «атакует вас/имя».
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attacking: Option<String>,
    /// M25: следующая атака моба (epoch, мс) — прогресс-бар атаки.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attack_at: Option<i64>,
    /// M25: интервал атаки моба, мс (знаменатель бара).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attack_interval: Option<i64>,
    /// Незнакомые поля (например, ссылки на лут контента) сохраняются и
    /// не ломают десериализацию (§2.3, §11.2).
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// M25: состояние босса в списке монстров — текущая фаза по времени
/// боя (сервер — истина; аддитивно §11: поля нет у старого сервера).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BossState {
    pub phase: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyph: Option<String>,
    /// M25: имя навыка в касте (подготовка) — строка босса.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub casting: Option<String>,
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
    /// M17/F30a: +урон руны в оружии (аддитивно §6: `None` — у
    /// сервера без поля).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damage: Option<i64>,
    /// M17/F30g: определение руны — ключ кулдауна каста на клиенте.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub def_id: Option<String>,
    /// M18/F58f: настраиваемый знак руны — контент-поле `glyph`
    /// (параметры руны, `stats.glyph`); латинская буква `a..=z`/`A..=Z`
    /// — глиф шрифта Dwarven Runes. Аддитивно §11: поля нет у сервера
    /// или руны → клиент фолбэчит стабильным хешем имени руны.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyph: Option<String>,
    /// M24: класс руны (`stats.class`: warrior|archer|mage) — каст
    /// и авто-приём только своему классу; аддитивно §11: поля нет у
    /// сервера/старой записи → любому классу.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// M18/F58f: раритет вставленного предмета — у самоцвета символ
    /// в отображении зависит от него (предмет и слот). Сервер кладёт
    /// при инкрустации (§8); аддитивно §11: поля нет → клиент
    /// показывает слот прежним символом ◈.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rarity: Option<String>,
    /// M18/F58f: подпись занятого слота — что даёт вставка (урон/шанс/
    /// бафф/эффект/трата; для самоцвета — атрибут). Сервер формирует
    /// при инкрустации (§8 — сервер-истина); аддитивно §11: старый
    /// сервер/старые записи без поля → клиент показывает без подписи.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Vec<String>>,
}

/// Сокет посоха: `rune: null` — свободный (`◈◈○` в UI, ФР-09).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Socket {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rune: Option<RuneInfo>,
    /// M18/F58 (ADR-012): тип слота — что он принимает (`rune` | `gem`).
    /// Аддитивно §11: старый сервер поля не шлёт → клиент фолбэчит по
    /// типу предмета (прежнее поведение: броня — самоцветы,
    /// оружие/посох — руны).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepts: Option<String>,
}

/// M18/F47/ADR-010: бонус предмета — атрибут, параметр или навык
/// (`kind`: `attr` | `param` | `skill`); `pct` — проценты, только у
/// `param` (суммируются к итогу).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemBonus {
    pub kind: String,
    pub id: String,
    pub value: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pct: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Предмет инвентаря/экипировки/трупа (§6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemSnapshot {
    pub uid: String,
    /// Тип: weapon|armor|staff|potion|scroll|rune|material|quest (открытая строка).
    #[serde(rename = "type")]
    pub item_type: String,
    pub name: String,
    /// M39: текстовое описание для карточки/тултипа; аддитивно §11.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
    /// M17/F30g: определение предмета — ключ кулдауна использования
    /// на клиенте (аддитивно §6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub def_id: Option<String>,
    /// M17/F30b: слот предмета (main_hand/off_hand/head/…; ring/artifact —
    /// псевдослоты пар); `None` — не экипируется (открытая строка §6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    /// M17/F30b: руки (2 — двуручное: занимает обе руки и блокирует
    /// левую); `None` → 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hands: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qty: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gold_value: Option<i64>,
    /// M18/F47/ADR-010: бонусы предмета (контент + инстанс) — аддитивно §11.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bonuses: Option<Vec<ItemBonus>>,
    /// M28: контейнер инвентаря — `backpack`, `bags` (слоты под сумки)
    /// или uid надетой сумки; аддитивно §11 (старый сервер не шлёт).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// M28: позиция внутри контейнера (`0..ёмкость-1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_index: Option<i32>,
    /// M28: ёмкость сумки (контент `bag_slots`) — только для `type=bag`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bag_slots: Option<u32>,
    /// M28: какие типы принимает сумка (пусто — любые).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bag_types: Option<Vec<String>>,
    /// M28/B8: тип слота под сумку (`reagent`, `quiver`, `keyring`);
    /// `None` — обычная сумка.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bag_kind: Option<String>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Навык персонажа (§6; `name` опционален — tz-снапшот шлёт только `id`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillSnapshot {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// M14/F17: описание эффекта из контента (тултип/карточка в UI).
    /// Опционально: серверы без контента обходят поле вовсе.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// M15/F28: группа навыка (skills.md §3: weapon/defense/magic/
    /// stealth/practice/social); None — сервер без групп.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub rank: u32,
    pub xp: i64,
    pub xp_to_next: i64,
    /// M31: потолок ранга по уровню персонажа (боевые/защитные/
    /// магические — не более 2 рангов за уровень); `None` — навык без
    /// такого ограничения. Аддитивно §6/§11.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank_cap: Option<u32>,
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
    /// R12 (§6): незнакомая фаза — не роняем разбор списка квестов.
    #[serde(other)]
    Unknown,
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

/// M18/F54: подтверждение выбора камня усиления (`cmd/item.use` ack):
/// uid выбранного камня и его качество в процентных пунктах
/// (`stats.quality`; −5 малый / 0 средний / +5 большой).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoneSelected {
    pub stone_uid: String,
    /// Процентные пункты к шансу заточки до затухания 1/(1+level).
    pub quality: i64,
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
    /// M18/F36: человекочитаемая строка награды из контента
    /// (`reward_label`), аддитивно §6.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_label: Option<String>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

// ─── Состояния снапшота и социальных событий ─────────────────────────────────

/// Профессия персонажа (`cmd/craft.list`, `evt/craft/profession`, §7–§8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfessionSnapshot {
    pub id: String,
    pub rank: u32,
    /// M21: уровень профессии (0..max_level) — вторая шкала; ранг
    /// определяется границами уровней (кратно rank_span).
    #[serde(default)]
    pub level: u32,
    pub xp: i64,
    pub xp_to_next: i64,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// M35: публичный осмотр персонажа (ПКМ) — только разрешённое:
/// уровень, класс, экипировка и изученные профессии.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharInspectView {
    pub id: String,
    pub name: String,
    pub level: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default)]
    pub equip: Vec<InspectItem>,
    #[serde(default)]
    pub professions: Vec<ProfessionSnapshot>,
}

/// M35: надетый предмет в карточке осмотра (слот + снимок вещи).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InspectItem {
    pub slot: String,
    pub item: ItemSnapshot,
}

/// M18/F58d (ADR-012 §6): активный бафф персонажа — руна-приём
/// наложила эффект (стак + таймер). `kind` — идентификатор (ключ
/// перевода/кэша), `title` — отображаемое название (сервер — истина,
/// §8), `stacks` — от1 до `max_stacks` из параметров руны.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuffState {
    pub kind: String,
    pub title: String,
    pub stacks: u32,
    /// Epoch-секунды снятия.
    pub expires_at: i64,
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

/// M15/F26: производные характеристики — вкладка «Параметры» окна
/// персонажа. Чистое представление серверных формул (skills.md §1, §A/B);
/// additive §11 — `None` у серверов без M15.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DerivedParams {
    /// Точность, % (шанс попадания).
    pub accuracy: i64,
    /// Физ. урон, мин (оружие/база + модификатор силы).
    pub damage_min: i64,
    /// Физ. урон, макс.
    pub damage_max: i64,
    /// Шанс крит. удара, %.
    pub crit_chance: i64,
    /// Крит. удар — бонус урона, %.
    pub crit_damage: i64,
    /// Броня, единиц (сумма статов экипировки).
    pub armor: i64,
    /// Уклон, %.
    pub dodge: i64,
    /// Парирование — контрудар, мин урона.
    pub parry_min: i64,
    /// Парирование — контрудар, макс урона.
    pub parry_max: i64,
    /// Шанс парирования, %.
    pub parry_chance: i64,
    /// Блок щитом — снижение, мин урона.
    pub block_min: i64,
    /// Блок щитом — снижение, макс урона.
    pub block_max: i64,
    /// Шанс блока щитом, %.
    pub block_chance: i64,
    /// Магическая защита, единиц.
    pub magic_defense: i64,
    /// Магический уклон, %.
    pub magic_dodge: i64,
    /// Сопротивление магии, %.
    pub magic_resist: i64,
    /// Рассеивание магии, %.
    pub magic_dispel: i64,
    /// Маг. крит — бонус урона, %.
    pub magic_crit_damage: i64,
    /// Шанс маг. крита, %.
    pub magic_crit_chance: i64,
}

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
    /// M15/F24: уровень персонажа (`None` — сервер без уровня, шкалу
    /// не показываем; аддитивно §11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u32>,
    /// M15/F24: весь опыт за убийства.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_xp: Option<i64>,
    /// M15/F24: опыт в текущем уровне.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xp_in_level: Option<i64>,
    /// M15/F24: опыт до следующего уровня (`0` — максимум).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xp_to_next: Option<i64>,
    /// M15/F26: производные характеристики окна «Параметры»
    /// (`None` — сервер без M15, вкладка покажет заглушку).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<DerivedParams>,
    /// M17/F30b: класс персонажа (`warrior`/`archer`/`mage`) —
    /// правила экипировки и ресурс боя (F30e); аддитивно §6.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// M18/F34: пол (`male`/`female`) — задаётся при создании, диалоги
    /// NPC знают пол (F35); старый сервер без поля → `male` (§11).
    #[serde(default = "default_gender")]
    pub gender: String,
    #[serde(default)]
    pub equip: EquipMap,
    #[serde(default)]
    pub inventory: Vec<ItemSnapshot>,
    #[serde(default)]
    pub crime: CrimeState,
    #[serde(default)]
    pub quests: Vec<QuestState>,
    /// M18/F58d (ADR-012 §6): активные баффы рун (стак + таймер);
    /// старый сервер без поля → пусто (§11).
    #[serde(default)]
    pub buffs: Vec<BuffState>,
    #[serde(flatten, default)]
    pub extra: JsonMap,
}

/// Фолбэк пола для снапшотов старых серверов (M18/F34, §11).
fn default_gender() -> String {
    "male".to_string()
}

/// Сводка персонажа в списке аккаунта (§5 `characters[]`, ФР-02).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CharacterSummary {
    pub id: String,
    pub name: String,
    /// Класс из `char.create` (ADR-005, аддитивно): `warrior`/`rogue`/`mage`.
    /// Опционально: серверы без поля → UI рисует нейтральный цвет.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
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

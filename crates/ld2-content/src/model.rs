//! Схема контента ld2 (M27/E1): типы всех файлов каталога `content/`.
//!
//! Единая схема для сервера (сид/hot reload) и редактора `ld2-editor`:
//! неизвестные поля сохраняются в `extra` (`serde(flatten)`) — редактор
//! не теряет данные при пересохранении, сервер продолжает «съедать»
//! расширения контента (§6/§11 контракта).

use ld2_protocol::types::{Entity, Exit, ItemBonus, Requires};
use serde::{Deserialize, Serialize};

/// Строка content-файла: выход хранит целевую локацию (`to`) — на проводе
/// её быть не должно (§6 [`Exit`] без `to`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentExit {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub dir: Option<String>,
    /// Целевая локация (только БД/контент, в снапшот не попадает).
    pub to: String,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ContentExit {
    /// Проводной вид выхода (§6): без `to`.
    pub fn to_wire(&self) -> Exit {
        Exit {
            id: self.id.clone(),
            label: self.label.clone(),
            dir: self.dir.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentLocation {
    pub id: String,
    pub name: String,
    /// Единственная спавн-точка новых персонажей (→ `meta.spawn_location`).
    #[serde(default)]
    pub spawn: bool,
    #[serde(default)]
    pub flags: Vec<String>,
    pub text: String,
    #[serde(default)]
    pub exits: Vec<ContentExit>,
    #[serde(default)]
    pub entities: Vec<Entity>,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Навык из `skills.json` (skills.md §3: id + имя; группа — для UI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentSkill {
    pub id: String,
    pub name: String,
    pub group: String,
    /// M14/F17: описание эффекта для UI (тултип/карточка навыка).
    /// `default` — старый skills.json без поля продолжает грузиться.
    #[serde(default)]
    pub description: String,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Определение предмета из `items.json` (§6 `ItemSnapshot`-ядро).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentItemDef {
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub name: String,
    pub rarity: String,
    #[serde(default)]
    pub upgrade: i32,
    #[serde(default)]
    pub stats: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub requires: Option<Requires>,
    #[serde(default)]
    pub gold_value: i64,
    /// Стартовое количество стака при взятии/выдаче.
    #[serde(default)]
    pub qty: Option<i64>,
    /// Навык, который качается убийством этим оружием (skills.md §2.3);
    /// `None` → `unarmed`.
    #[serde(default)]
    pub skill: Option<String>,
    /// Количество сокетов (посохи, ADR-002) — создаются при вставке.
    #[serde(default)]
    pub sockets: u32,
    /// M18/F58 (ADR-012, уточнение заказчика): сколько слотов
    /// принимают руны — **отдельное поле определения** (как
    /// `sockets`), а не в `stats`. `None` — не указано → дефолт по
    /// типу: броня 0, оружие/посох — все слоты.
    #[serde(default)]
    pub rune_sockets: Option<i32>,
    /// M17/F30g: кулдаун использования/скорость атаки, с
    /// (дефолт 1.0 — «по умолчанию 1 сек»).
    #[serde(default)]
    pub cooldown_s: Option<f64>,
    /// M17/F30b: слот, куда предмет надевается (main_hand/off_hand/
    /// head/…/псевдослоты пар ring|artifact); `None` — не экипируется.
    #[serde(default)]
    pub slot: Option<String>,
    /// M17/F30b: руки (2 — двуручное: занимает обе руки и блокирует
    /// левую); `None` → 1.
    #[serde(default)]
    pub hands: Option<i32>,
    /// M28: ёмкость сумки (число слотов); `None` — предмет не сумка.
    #[serde(default)]
    pub bag_slots: Option<u32>,
    /// M28: типы предметов, которые принимает сумка (например,
    /// `["herb"]` — сумка травника); `None`/пусто — любые.
    #[serde(default)]
    pub bag_types: Option<Vec<String>>,
    /// M28/B8: тип спец-слота под сумку (`general` — обычная,
    /// `reagent`, `quiver`, `keyring`); `None` — обычная.
    #[serde(default)]
    pub bag_kind: Option<String>,
    /// M18/F47: бонусы предмета (ADR-010) — атрибут/параметр/навык.
    #[serde(default)]
    pub bonuses: Vec<ItemBonus>,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Профессия из `professions.json` (skills.md §5.1: первичная/вторичная).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentProfession {
    pub id: String,
    pub name: String,
    /// `primary` — занимает слот `profession_limit`; `secondary` — нет.
    #[serde(rename = "type", default = "default_primary")]
    pub prof_type: String,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn default_primary() -> String {
    "primary".into()
}

/// Инструмент рецепта: предмет с этим `item_type` в инвентаре (skills.md §5.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentTool {
    #[serde(rename = "type")]
    pub item_type: String,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Требования рецепта (открытый объект §6: attr/skill — сервер читает).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentRequires {
    #[serde(default)]
    pub attr: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub skill: std::collections::BTreeMap<String, i64>,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Рецепт из `recipes.json` (skills.md §5.4 — payload `cmd/craft.recipes`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentRecipe {
    pub id: String,
    pub name: String,
    pub profession: String,
    #[serde(default = "one")]
    pub rank: i32,
    /// Станок, который должен быть в локации; `None` — без станка.
    #[serde(default)]
    pub station: Option<String>,
    #[serde(default)]
    pub tool: Option<ContentTool>,
    #[serde(default)]
    pub requires: ContentRequires,
    #[serde(default)]
    pub inputs: Vec<ContentInput>,
    #[serde(default = "one")]
    pub time_s: i32,
    #[serde(default)]
    pub xp: ContentRecipeXp,
    pub output: ContentOutput,
    #[serde(default)]
    pub station_label: Option<String>,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn one() -> i32 {
    1
}

/// Ингредиент: `qty` штук с `item_type` в инвентаре.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentInput {
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default = "one_i64")]
    pub qty: i64,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn one_i64() -> i64 {
    1
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentRecipeXp {
    #[serde(default)]
    pub profession: i64,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentOutput {
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default = "one_i64")]
    pub qty: i64,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Квест из `quests.json` (ФР-12).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentQuest {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub giver: Option<String>,
    #[serde(default)]
    pub goals: Vec<ContentGoal>,
    #[serde(default)]
    pub reward: ContentReward,
    #[serde(default)]
    pub reward_label: Option<String>,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Цель квеста: `kill <target>` — убийства сущности с данным id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentGoal {
    #[serde(rename = "type")]
    pub goal_type: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default = "one_i64")]
    pub need: i64,
    #[serde(default)]
    pub text: String,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Награда квеста: XP в пул + предметы; M21 — опционально
/// **повышение ранга профессии** (`rank_up: "<profession_id>"`):
/// та же граница уровней, что у тренера, но без золота.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentReward {
    #[serde(default)]
    pub xp: i64,
    #[serde(default)]
    pub items: Vec<ContentInput>,
    #[serde(default)]
    pub rank_up: Option<String>,
    /// Неизвестные поля схемы (открытый контент, §6/§11): сохраняются
    /// при пересохранении — редактор не теряет данные сервера.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ─── dialogs.json / shops.json (E5.2: отдельные файлы каталога) ───────────
//
// Сервер держит собственные типы (`ld2-server::dialogs`/`shops`) — здесь
// тонкие вьюхи для редактора: валидация ссылок и подписи в списках.
// Неизвестные поля сохраняются в `extra` (файлы правятся сырым JSON).

/// Корень `content/dialogs.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentDialogs {
    #[serde(default)]
    pub npcs: Vec<ContentDialogNpc>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Диалог одного NPC.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentDialogNpc {
    pub id: String,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub lines: Vec<ContentDialogLine>,
    #[serde(default)]
    pub training: Vec<ContentTrainingEntry>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Строка диалога (ветка): текст + реплики-переходы.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentDialogLine {
    pub id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub when: Option<ContentDialogWhen>,
    #[serde(default)]
    pub quest: Option<String>,
    #[serde(default)]
    pub every_s: Option<i64>,
    #[serde(default)]
    pub replies: Vec<ContentDialogReply>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Реплика игрока: переход `next` или действие `action`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentDialogReply {
    pub id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub when: Option<ContentDialogWhen>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Условия видимости строки/реплики (AND; незаданные ключи не проверяются).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentDialogWhen {
    #[serde(default)]
    pub gender: Option<String>,
    #[serde(default)]
    pub level: Option<ContentMin>,
    #[serde(default)]
    pub attr: Option<ContentNamedMin>,
    #[serde(default)]
    pub skill: Option<ContentRankMin>,
    #[serde(default)]
    pub profession: Option<ContentRankMin>,
    #[serde(default)]
    pub item: Option<ContentItemQty>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// `when.level = {"min": 5}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentMin {
    pub min: i64,
}

/// `when.attr = {"name": "str", "min": 5}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentNamedMin {
    pub name: String,
    pub min: i64,
}

/// `when.skill/profession = {"id": "...", "min_rank": 1}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentRankMin {
    pub id: String,
    #[serde(default = "one_i64")]
    pub min_rank: i64,
}

/// `when.item` / плата предметом: `{"def_id": "...", "qty": 1}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentItemQty {
    pub def_id: String,
    #[serde(default = "one_i64")]
    pub qty: i64,
}

/// Обучение навыку у NPC (`training[]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentTrainingEntry {
    pub skill: String,
    #[serde(default)]
    pub points: i64,
    #[serde(default)]
    pub gold: i64,
    #[serde(default)]
    pub items: Vec<ContentItemQty>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Корень `content/shops.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentShops {
    #[serde(default)]
    pub npcs: Vec<ContentShopNpc>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Магазин одного NPC.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentShopNpc {
    pub id: String,
    #[serde(default = "one_f64")]
    pub markup: f64,
    #[serde(default)]
    pub currencies: Vec<ContentShopCurrency>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Валютная вкладка магазина.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentShopCurrency {
    pub currency: String,
    #[serde(default = "one_f64")]
    pub rate: f64,
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn one_f64() -> f64 {
    1.0
}

/// Весь контент каталога (для версии, сида и редактора).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameContent {
    pub locations: Vec<ContentLocation>,
    pub skills: Vec<ContentSkill>,
    pub items: Vec<ContentItemDef>,
    pub professions: Vec<ContentProfession>,
    pub recipes: Vec<ContentRecipe>,
    pub quests: Vec<ContentQuest>,
}

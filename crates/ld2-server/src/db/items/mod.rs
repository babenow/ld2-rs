//! Предметы: инстансы инвентаря/экипировки и взятый статический лут
//! (M12.4, ФР-06/07, D27 — изменения персистятся немедленно).

use serde_json::Value;
use sqlx::PgPool;

mod consume;
mod containers;
mod grant;
mod mutate;
mod query;

pub use consume::*;
pub use containers::*;
pub use grant::*;
pub use mutate::*;
pub use query::*;

/// F29a (M16): деньги — предметы-монеты (тип контента; стак в
/// инвентаре, лут с мобов, оплата покупок).
pub const MONEY_TYPE: &str = "money";

/// Инстанс предмета в инвентаре персонажа.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ItemRow {
    pub uid: String,
    pub def_id: String,
    pub qty: i32,
    pub upgrade: i32,
    pub equipped_slot: Option<String>,
    /// Сокеты посоха: `[{index, rune?, def_id?}]` (ADR-002; `def_id` —
    /// служебное поле для извлечения, в §6 не попадает).
    pub sockets: serde_json::Value,
    /// M18/F47: бонусы инстанса (ADR-010); `NULL` — нет своих бонусов.
    pub bonuses: Option<Value>,
    /// M28: контейнер (`backpack`, `bags`, uid надетой сумки).
    pub container: String,
    /// M28: позиция внутри контейнера; `NULL` — старая строка без позиции.
    pub slot_index: Option<i32>,
}

/// Определение из сида (`content/items.json` → `item_defs`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ItemDefRow {
    pub id: String,
    pub item_type: String,
    pub name: String,
    pub rarity: String,
    pub upgrade: i32,
    pub stats: Value,
    pub requires: Option<Value>,
    pub gold_value: i32,
    pub qty: i32,
    /// Количество сокетов посоха (ADR-002) — ленивая инициализация.
    pub sockets: i32,
    /// M18/F58 (ADR-012, уточнение заказчика): сколько слотов
    /// принимают руны; `NULL` — не указано → дефолт по типу предмета.
    pub rune_sockets: Option<i32>,
    /// M17/F30b: слот предмета (main_hand/off_hand/head/…; ring/artifact —
    /// псевдослоты пар); `None` — не экипируется.
    pub slot: Option<String>,
    /// M17/F30b: руки (2 — двуручник: занимает обе, блокирует левую).
    pub hands: i32,
    /// Навык оружия (skills.md §2.3; идёт в снапшот `extra["skill"]`).
    pub skill: Option<String>,
    /// M17/F30g: кулдаун использования/скорость атаки, с (контент,
    /// дефолт 1.0).
    pub cooldown_s: f64,
    /// M18/F47: бонусы из контента (ADR-010); `NULL` — их нет.
    pub bonuses: Option<Value>,
    /// M28: ёмкость сумки, слотов; `NULL` — обычный предмет.
    pub bag_slots: Option<i32>,
    /// M28: типы, которые принимает сумка (пусто — любые).
    pub bag_types: Option<Vec<String>>,
}

/// M24: инвентарь персонажа с данными определений (правая колонка
/// админ-окна): uid/деф/количество + имя/тип/редкость одним запросом.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AdminInvRow {
    pub uid: String,
    pub def_id: String,
    pub qty: i32,
    pub name: String,
    pub item_type: String,
    pub rarity: String,
}

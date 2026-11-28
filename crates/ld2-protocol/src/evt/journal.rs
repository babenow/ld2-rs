//! Журнальные события мира: выброс/истечение предметов и добыча руды.
//!
//! Вынесено из `evt.rs` (RF0): `evt.rs` — бюджет прод-строк, новые
//! события журнала живут здесь и реэкспортируются.

use serde::{Deserialize, Serialize};

/// M20: предмет в локации исчез (TTL выброса) — журнал «{name} исчез».
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemExpired {
    /// Id наземного предмета (на будущее; журналу нужно name).
    pub uid: String,
    pub name: String,
}

/// M20: выброс предмета в локации — журнал у всех, кто в локации
/// (§8, аддитивно): «{by} выбросил(а) …» / «Вы выбросили …».
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemDropped {
    /// Кто бросил (`char_id`) — для строки «Вы …» у себя.
    pub char_id: String,
    /// Имя персонажа (для «{by} выбросил(а)»).
    pub by_name: String,
    /// `male`|`female` — выбор «выбросил/выбросила».
    pub gender: String,
    pub qty: i64,
    /// Название предмета.
    pub item_name: String,
}

/// M39: успешная добыча руды — общая строка журнала всем в локации
/// («{имя} добыл(а) руду»; свой персонаж — «Вы добыли руду»).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceGathered {
    /// Кто добыл (`char_id`) — для строки «Вы …» у себя.
    pub char_id: String,
    /// Имя персонажа (для «{by} добыл(а) руду»).
    pub by_name: String,
    /// `male`|`female` — выбор «добыл/добыла».
    pub gender: String,
}

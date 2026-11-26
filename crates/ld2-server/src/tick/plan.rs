//! Тик-луп мира (D31, M12.5): атаки персонажей и агрессивных мобов,
//! смерти, XP убийств (skills.md §2.3) и мана-реген (§2.4, конфиг
//! `[gameplay.magic.mana]`). Адресация событий — через реестр соединений
//! мира (§2.4: порядок внутри канала сохраняется).
//!
//! Разложено по фазам: `run` (цикл и применение плана), `combat` (план
//! боя, смерти, награды), `jobs` (работы, DoT, баффы), `expire`
//! (истечение сущностей), `plan` (структуры плана и события).

/// Запланированный урон моба персонажу.
#[derive(Debug)]
pub(super) struct MobHit {
    pub(super) char_id: String,
    pub(super) amount: i64,
}

/// Запланированный XP убийства. `skill` — `None`: навык берётся из
/// экипированного оружия при применении; `Some` — школа заклинания (M12.5b).
#[derive(Debug)]
pub(crate) struct KillXp {
    pub(crate) char_id: String,
    pub(crate) amount: i64,
    pub(crate) skill: Option<String>,
    /// id убитой сущности — прогресс квестов типа `kill` (ФР-12).
    pub(crate) target: Option<String>,
}

/// M18/F58d: отложенная работа авто-приёма руны — применяется в
/// `tick_once` (у плана нет доступа к БД): расход зарядов + бафф.
pub(super) struct RuneWork {
    pub(super) char_id: String,
    /// uid оружия-носителя (запись charges).
    pub(super) staff_uid: String,
    pub(super) socket_index: u32,
    pub(super) new_charges: i64,
    /// Наложить бафф персонажа (`stats.buff` руны).
    pub(super) buff: Option<crate::world::RuneBuff>,
}

#[derive(Default)]
pub(super) struct Plan {
    /// (адресат — char_id, JSON конверта).
    pub(super) events: Vec<(String, String)>,
    pub(super) mob_hits: Vec<MobHit>,
    pub(super) xp: Vec<KillXp>,
    /// M17/F30e: нанесённый персонажами урон → прирост ярости (БД
    /// применяется в `tick_once`, у плана нет доступа к БД).
    pub(super) char_hits: Vec<(String, i64)>,
    /// M18/F58d: сработавшие авто-приёмы рун (БД — в `tick_once`).
    pub(super) rune_work: Vec<RuneWork>,
}

pub(super) fn push_event<T: serde::Serialize>(plan: &mut Plan, addr: &str, ty: &str, payload: &T) {
    let text = event_text(ty, payload);
    plan.events.push((addr.to_string(), text));
}

pub(super) fn push_text<T: serde::Serialize>(
    plan: &mut Vec<(String, String)>,
    addr: &str,
    ty: &str,
    payload: &T,
) {
    let text = event_text(ty, payload);
    plan.push((addr.to_string(), text));
}

pub(super) fn event_text<T: serde::Serialize>(ty: &str, payload: &T) -> String {
    crate::session::event_text(ty, None, serde_json::to_value(payload).unwrap_or_default())
}

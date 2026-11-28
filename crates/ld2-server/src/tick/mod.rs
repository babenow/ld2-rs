//! Тик-луп мира (D31, M12.5): атаки персонажей и агрессивных мобов,
//! смерти, XP убийств (skills.md §2.3) и мана-реген (§2.4, конфиг
//! `[gameplay.magic.mana]`). Адресация событий — через реестр соединений
//! мира (§2.4: порядок внутри канала сохраняется).
//!
//! Разложено по фазам: `run` (цикл и применение плана), `combat` (план
//! боя, смерти, награды), `jobs` (работы, DoT, баффы), `expire`
//! (истечение сущностей), `plan` (структуры плана и события).

mod combat;
mod expire;
mod gather;
mod jobs;
mod plan;
mod run;

pub(crate) use combat::{grant_kill_xp, mob_kill_effects};
pub(crate) use jobs::quest_kill_progress;
pub(crate) use plan::KillXp;
pub use run::{init, run};

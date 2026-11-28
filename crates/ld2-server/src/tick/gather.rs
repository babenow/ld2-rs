//! M39: проверка места и бонусы добычи ресурсов.

use crate::db;
use crate::state::SharedState;

/// M39: предметы-модификаторы добычи.
const MASTER_PICKAXE: &str = "pickaxe_master";
const MINER_LAMP: &str = "artifact_lamp";

/// M39: хватит ли места под предмет (стак или свободный слот).
pub(super) async fn gather_fits(
    state: &SharedState,
    char_id: &str,
    def_id: &str,
    slots: u32,
) -> bool {
    let pool = &state.pool;
    if db::items::stackable_exists(pool, char_id, def_id)
        .await
        .unwrap_or(false)
    {
        return true;
    }
    db::items::has_space(pool, char_id, i64::from(slots))
        .await
        .unwrap_or(false)
}

/// M39: бонусы добычи от «Кирки мастера» (в инвентаре) и «Лампы
/// шахтёра» (надета в артефакт). → (множитель камней, прибавка, руды).
pub(super) async fn gather_bonuses(state: &SharedState, char_id: &str) -> (f64, f64, i64) {
    let cfg = &state.config.gameplay.professions;
    let master = db::items::has_def(&state.pool, char_id, MASTER_PICKAXE)
        .await
        .unwrap_or(false);
    let lamp = db::items::has_equipped_def(&state.pool, char_id, MINER_LAMP)
        .await
        .unwrap_or(false);
    let mult = if master {
        cfg.mining_master_gem_mult
    } else {
        1.0
    };
    let add = if lamp { cfg.mining_lamp_gem_bonus } else { 0.0 };
    let qty = if lamp && crate::world::roll_chance(cfg.mining_lamp_double_ore_chance) {
        2
    } else {
        1
    };
    (mult, add, qty)
}

//! M39: проверка места и бонусы добычи ресурсов.

use crate::db;
use crate::state::SharedState;

/// M39: предметы-модификаторы добычи/разделки.
const MASTER_PICKAXE: &str = "pickaxe_master";
const MINER_LAMP: &str = "artifact_lamp";
const MASTER_SKINNING_KNIFE: &str = "skinning_knife_master";
const HUNTER_FETISH: &str = "artifact_hunter";

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
/// шахтёра» (надета в артефакт). → (множитель камней, прибавка, руды,
/// uid только что привязанных кирок).
pub(super) async fn gather_bonuses(
    state: &SharedState,
    char_id: &str,
) -> (f64, f64, i64, Vec<String>) {
    let cfg = &state.config.gameplay.professions;
    let master = db::items::has_def(&state.pool, char_id, MASTER_PICKAXE)
        .await
        .unwrap_or(false);
    let lamp = db::items::has_equipped_def(&state.pool, char_id, MINER_LAMP)
        .await
        .unwrap_or(false);
    // M39: первое применение кирки накладывает печать персональности.
    let bound = if master {
        db::items::bind_by_def(&state.pool, char_id, MASTER_PICKAXE)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
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
    (mult, add, qty, bound)
}

/// M39: бонусы снятия шкур от «Ножа мастера» (в инвентаре) и «Фетиша
/// охотника» (надет в артефакт). → (множитель, прибавка, uid только что
/// привязанных ножей).
pub(super) async fn skinning_bonuses(
    state: &SharedState,
    char_id: &str,
) -> (f64, f64, Vec<String>) {
    let cfg = &state.config.gameplay.professions;
    let master = db::items::has_def(&state.pool, char_id, MASTER_SKINNING_KNIFE)
        .await
        .unwrap_or(false);
    let hunter = db::items::has_equipped_def(&state.pool, char_id, HUNTER_FETISH)
        .await
        .unwrap_or(false);
    // M39: первое применение ножа накладывает печать персональности.
    let bound = if master {
        db::items::bind_by_def(&state.pool, char_id, MASTER_SKINNING_KNIFE)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mult = if master {
        cfg.skinning_master_loot_mult
    } else {
        1.0
    };
    let add = if hunter {
        cfg.skinning_hunter_loot_bonus
    } else {
        0.0
    };
    (mult, add, bound)
}

/// M39: полный снапшот предмета в `evt/item/state` (после печати).
pub(super) async fn full_item_state_event(
    state: &SharedState,
    char_id: &str,
    uid: &str,
) -> Option<String> {
    let row = db::items::find(&state.pool, uid, char_id).await.ok()??;
    let def = db::items::def(&state.pool, &row.def_id).await.ok()??;
    let payload = serde_json::json!({
        "uid": uid,
        "patch": { "item": crate::session::item_snapshot(&row, &def) }
    });
    let text = crate::session::event_text(ld2_protocol::names::EVT_ITEM_STATE, None, payload);
    (!text.is_empty()).then_some(text)
}

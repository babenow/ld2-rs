//! M33/M34: множители прогресса (премиум и будущие события/акции).

use crate::state::SharedState;

/// Опыт профессии с учётом всех множителей: премиум
/// (`[gameplay.premium] profession_mult`, +50 % по умолчанию) ×
/// дополнительные множители (события/акции — задел, сейчас пусто).
/// Единая точка: крафт, сбор и снятие шкур.
pub async fn profession_xp(state: &SharedState, char_id: &str, base: i64) -> i64 {
    let premium =
        crate::db::accounts::premium_for_char(&state.pool, char_id, crate::time::now_epoch())
            .await
            .unwrap_or(false);
    let mult = crate::systems::profession_xp_multiplier(
        premium,
        state.config.gameplay.premium.profession_mult,
        &[],
    );
    crate::systems::premium_gain(base, mult)
}

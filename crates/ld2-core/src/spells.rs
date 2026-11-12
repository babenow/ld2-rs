//! M17/F30f: перевод названий заклинаний по контент-id (`stats.spell`).
//! Слова и «чем бьёт» — общий реестр в [`ld2_protocol::spells`]
//! (серверная сторона, §8).

use crate::l10n::Key;

/// Kлюч перевода названия заклинания по его контент-id.
pub fn spell_key(id: &str) -> Option<Key> {
    match id {
        "fire_dart" => Some(Key::SpellFireDart),
        "frost_bolt" => Some(Key::SpellFrostBolt),
        "storm_bolt" => Some(Key::SpellStormBolt),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M17/F30f: каждому известному заклинанию соответствует ключ.
    #[test]
    fn every_known_spell_has_key() {
        for id in ["fire_dart", "frost_bolt", "storm_bolt"] {
            assert!(spell_key(id).is_some(), "имя: {id}");
        }
        assert!(spell_key("mystery").is_none(), "неизвестное — None");
    }
}

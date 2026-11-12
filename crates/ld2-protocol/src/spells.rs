//! M17/F30f: заклинания — магические слова и «чем бьёт» (скрытый
//! параметр для строк лога боя, §8). Общий реестр: сервер строит
//! тексты, клиент переводит названия (перевод — `ld2_core::spells`).

/// Магические слова заклинания по контент-id (`stats.spell`) —
/// одинаковы на всех языках.
pub fn spell_words(id: &str) -> Option<&'static str> {
    match id {
        "fire_dart" => Some("Farta Lussen"),
        "frost_bolt" => Some("Neris Vael"),
        "storm_bolt" => Some("Tor Keldon"),
        _ => None,
    }
}

/// Чем бьёт заклинание — творительный для русского текста лога (§8).
pub fn spell_hits_as(id: &str) -> Option<&'static str> {
    match id {
        "fire_dart" => Some("струёй пламени"),
        "frost_bolt" => Some("ледяной стрелой"),
        "storm_bolt" => Some("ударом шторма"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M17/F30f: у каждого известного заклинания есть слова и удар.
    #[test]
    fn every_known_spell_is_complete() {
        for id in ["fire_dart", "frost_bolt", "storm_bolt"] {
            assert!(
                !spell_words(id).unwrap_or_default().is_empty(),
                "слова: {id}"
            );
            assert!(
                !spell_hits_as(id).unwrap_or_default().is_empty(),
                "чем бьёт: {id}"
            );
        }
        assert!(spell_words("mystery").is_none(), "неизвестное — None");
    }
}

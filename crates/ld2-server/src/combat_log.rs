//! M17/F30f: тексты лога боя (§8) — магические слова заклинаний,
//! скрытый параметр «чем бьёт», выделение жирным (`**...**` — сегменты
//! раскрашивает клиент) и обращение «вы» для самого персонажа.
//!
//! Падежи имён сущностей не склоняются (как и раньше: «Вы бьёте Волк») —
//! открытый контент имён не даёт морфологии.

use ld2_protocol::spells;

/// Чем бьёт обычным оружием — творительный по навыку/типу предмета.
pub fn weapon_hits_as(skill: Option<&str>, item_type: &str) -> &'static str {
    if item_type == "staff" {
        return "посохом";
    }
    match skill {
        Some("sword") => "мечем",
        Some("dagger") => "кинжалом",
        Some("axe") => "топором",
        Some("polearm") => "алебардой",
        Some("twohand") => "двуручным мечом",
        Some("bow") => "из лука",
        _ => "руками",
    }
}

/// Строка удара заклинанием: кастеру — `{имя}: **слова**, вы по…`,
/// наблюдателям — `{имя}: **слова**. {имя} по…`; урон всегда жирный.
/// M18/F48: предлог «по» вместо «бьёт» (формат `[имя] по [цель]
/// [чем_бьёт] [урон]`).
pub fn spell_hit_text(
    spell: &str,
    caster: &str,
    target: &str,
    damage: i64,
    to_caster: bool,
) -> String {
    let hits_as = spells::spell_hits_as(spell).unwrap_or("силой заклинания");
    match spells::spell_words(spell) {
        Some(words) if to_caster => {
            format!("{caster}: **{words}**, вы по {target} {hits_as} **{damage}**")
        }
        Some(words) => {
            format!("{caster}: **{words}**. {caster} по {target} {hits_as} **{damage}**")
        }
        None if to_caster => format!("Вы по {target} {hits_as} **{damage}**"),
        None => format!("{caster} по {target} {hits_as} **{damage}**"),
    }
}

/// Обычный удар/навык: кастеру — «Вы по…», наблюдателям — имя;
/// урон жирный. M18/F48: формат `[имя] по [цель] [чем_бьёт] [урон]`.
pub fn melee_hit_text(
    source: &str,
    target: &str,
    hits_as: &str,
    damage: i64,
    to_caster: bool,
) -> String {
    if to_caster {
        format!("Вы по {target} {hits_as} **{damage}**")
    } else {
        format!("{source} по {target} {hits_as} **{damage}**")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M17/F30f/M18/F48: форматы кастера/наблюдателя — предлог «по»
    /// (формат `[имя] по [цель] [чем_бьёт] [урон]`), слова и урон жирные.
    #[test]
    fn spell_text_has_words_and_bold_damage() {
        let mine = spell_hit_text("fire_dart", "Bbnf", "Волк", 10, true);
        assert_eq!(
            mine,
            "Bbnf: **Farta Lussen**, вы по Волк струёй пламени **10**"
        );
        let theirs = spell_hit_text("fire_dart", "Bbnf", "Волк", 10, false);
        assert_eq!(
            theirs,
            "Bbnf: **Farta Lussen**. Bbnf по Волк струёй пламени **10**"
        );
        // Неизвестное заклинание — без слов, но с ударом и жирным уроном.
        let plain = spell_hit_text("mystery", "Bbnf", "Волк", 5, true);
        assert_eq!(plain, "Вы по Волк силой заклинания **5**");
    }

    #[test]
    fn melee_text_by_weapon() {
        assert_eq!(weapon_hits_as(Some("sword"), "weapon"), "мечем");
        assert_eq!(weapon_hits_as(Some("axe"), "weapon"), "топором");
        assert_eq!(weapon_hits_as(None, "staff"), "посохом");
        assert_eq!(weapon_hits_as(None, "weapon"), "руками");
        let mine = melee_hit_text("Bbnf", "Волк", "мечем", 7, true);
        assert_eq!(mine, "Вы по Волк мечем **7**");
        let theirs = melee_hit_text("Bbnf", "Волк", "мечем", 7, false);
        assert_eq!(theirs, "Bbnf по Волк мечем **7**");
    }
}

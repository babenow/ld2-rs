//! Чистые хелперы вьюпорта/клавиш, живущие вне `Root`: раскладка,
//! ОС, клампы карточек, спека клавиши. Вынесено из `root.rs` (RF3j).

/// Текущая ОС — для разрешения `secondary-` в Keystroke::parse_with_os.
/// M14/F21: русская/кириллическая раскладка по TIS-id и локализованному
/// имени (macOS: `com.apple.keylayout.Russian` + «Русская»/«Russian»;
/// Linux: id `ru`). Чистая функция — юнит-тест.
pub(crate) fn is_cyrillic_layout(id: &str, name: &str) -> bool {
    let id = id.to_lowercase();
    let name = name.to_lowercase();
    id == "ru" || id.contains("russ") || name.contains("russ") || name.contains("русск")
}

pub(crate) fn host_os() -> gpui::OperatingSystem {
    if cfg!(target_os = "macos") {
        gpui::OperatingSystem::Mac
    } else if cfg!(windows) {
        gpui::OperatingSystem::Windows
    } else {
        gpui::OperatingSystem::Linux
    }
}

/// M15/F25e: закрыть окно по mouse-up на scrim можно, только если
/// жест **начался** на scrim (настоящий «клик мимо») и это не
/// завершение перетаскивания/resize (gpui ещё держит active_drag).
pub(crate) fn scrim_closes_on_up(scrim_down: bool, has_active_drag: bool) -> bool {
    scrim_down && !has_active_drag
}

/// M15/F25d: верхний левый угол карточки окна персонажа — центр
/// вьюпорта + смещение drag (absolute-позиционирование как в F14:
/// без марджинов, «половинных» сдвигов центрирования нет).
pub(crate) fn char_card_origin(vw: f32, vh: f32, w: f32, h: f32, delta: (f32, f32)) -> (f32, f32) {
    (
        ((vw - w) / 2.0).max(0.0) + delta.0,
        ((vh - h) / 2.0).max(0.0) + delta.1,
    )
}

/// M15/F25c: кламп размера карточки окна персонажа — min 560×400,
/// max = вьюпорт − 40 px (но не уже min).
pub(crate) fn clamp_char_size(w: f32, h: f32, vw: f32, vh: f32) -> (f32, f32) {
    (
        w.clamp(560.0, (vw - 40.0).max(560.0)),
        h.clamp(400.0, (vh - 40.0).max(400.0)),
    )
}

/// Нажатие → спецификация (`alt+up`) для записи в config (ФР-19).
pub(crate) fn spec_from_keystroke(ks: &gpui::Keystroke) -> Option<String> {
    let name = ks.key.to_lowercase();
    if name.is_empty() {
        return None;
    }
    let mut spec = String::new();
    if ks.modifiers.alt {
        spec.push_str("alt+");
    }
    if ks.modifiers.control {
        spec.push_str("ctrl+");
    }
    if ks.modifiers.shift {
        spec.push_str("shift+");
    }
    if ks.modifiers.platform {
        spec.push_str("cmd+");
    }
    spec.push_str(&name);
    Some(spec)
}

/// M15: раскладки QWERTY ↔ ЙЦУКЕН (26 букв; одиночные символы хоткеев).
pub(crate) const QWERTY_CYRILLIC: [(char, char); 26] = [
    ('q', 'й'),
    ('w', 'ц'),
    ('e', 'у'),
    ('r', 'к'),
    ('t', 'е'),
    ('y', 'н'),
    ('u', 'г'),
    ('i', 'ш'),
    ('o', 'щ'),
    ('p', 'з'),
    ('a', 'ф'),
    ('s', 'ы'),
    ('d', 'в'),
    ('f', 'а'),
    ('g', 'п'),
    ('h', 'р'),
    ('j', 'о'),
    ('k', 'л'),
    ('l', 'д'),
    ('z', 'я'),
    ('x', 'ч'),
    ('c', 'с'),
    ('v', 'м'),
    ('b', 'и'),
    ('n', 'т'),
    ('m', 'ь'),
];

/// M15: приводит одиночный символ к «физическому» виду — кириллица
/// ЙЦУКЕН → латиница QWERTY, регистр не важен; прочие символы — как есть.
pub(crate) fn physical_key_char(ch: char) -> char {
    let lower = ch.to_ascii_lowercase();
    if lower.is_ascii_alphabetic() {
        return lower;
    }
    QWERTY_CYRILLIC
        .iter()
        .find(|(_, cyr)| *cyr == lower)
        .map(|(lat, _)| *lat)
        .unwrap_or(lower)
}

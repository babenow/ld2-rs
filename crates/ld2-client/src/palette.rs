//! Палитра UI gpui-клиента: тёмная/светлая тема из `config.theme` (ФР-19).
//!
//! Цвета — адаптация палитры §7.4 ТЗ (та же, что у egui-клиента); светлая
//! тема использует затемнённые варианты светлых строк (контраст AA, НФР).

/// Цветовая схема одной темы.
pub struct Palette {
    pub bg: u32,
    pub panel: u32,
    pub border: u32,
    pub text: u32,
    pub muted: u32,
    pub accent: u32,
    pub accent_text: u32,
    pub danger: u32,
    pub success: u32,
    pub warn: u32,
    /// Подсветка выделения/активной строки.
    pub selection: u32,
}

const DARK: Palette = Palette {
    bg: 0x1e1e24,
    panel: 0x2a2a30,
    border: 0x3a3a42,
    text: 0xeceff4,
    muted: 0x8a8f98,
    accent: 0x4a90d9,
    accent_text: 0xffffff,
    danger: 0xe06c75,
    success: 0x98c379,
    warn: 0xd19a66,
    selection: 0x363640,
};

const LIGHT: Palette = Palette {
    bg: 0xf5f5f7,
    panel: 0xffffff,
    border: 0xd0d0d6,
    text: 0x1e1e24,
    muted: 0x5a5e66,
    accent: 0x2f6fd0,
    accent_text: 0xffffff,
    danger: 0xb32d2d,
    success: 0x2e7d32,
    warn: 0x9a5b00,
    selection: 0xe4e4ea,
};

/// Палитра по строке темы из конфига (`dark` | `light`; прочее → dark).
pub fn palette(theme: &str) -> Palette {
    if theme == "light" {
        LIGHT
    } else {
        DARK
    }
}

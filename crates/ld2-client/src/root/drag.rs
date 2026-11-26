//! Перетаскивания: ghost-карточка предмета, состояние удержания строки
//! рюкзака и payload-drag'и окна/карточки/карты. Вынесено из `root.rs` (RF3j).

use super::*;

/// R-dnd: «карточка» предмета под курсором при перетаскивании
/// (ghost-вид on_drag — следует за мышью).
pub(crate) struct ItemDragGhost {
    pub name: String,
    /// Цвет рамки/текста — цвет редкости.
    pub color: u32,
    pub theme: String,
}

impl gpui::Render for ItemDragGhost {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let pal = crate::palette::palette(&self.theme);
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(self.color))
            .text_size(px(13.0))
            .text_color(rgb(self.color))
            .child(self.name.clone())
            .opacity(0.9)
    }
}

/// R-dnd: состояние удерживаемой строки списка рюкзака.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BpHold {
    pub uid: String,
    /// Индекс удерживаемой строки в исходном порядке.
    pub index: usize,
    /// Целевой индекс (landing).
    pub target: usize,
    /// Смещение строки под курсором, px.
    pub top: f32,
    /// Захват внутри строки (offset.y при старте), px.
    pub grab: f32,
}

/// M14/F15: перетаскивание окна за TitleBar (payload drag gpui).
#[derive(Debug)]
pub(crate) struct TitlebarDrag;

/// M16/F29b: перетаскивание карточки всплывающего окна (персонаж/
/// рюкзак) за шапку — общий payload drag оболочки `popup_window`.
#[derive(Debug)]
pub(crate) struct PopupDrag;

/// M16/F29b: изменение размера карточки всплывающего окна (уголок).
#[derive(Debug)]
pub(crate) struct PopupResize;

/// M26: панорама карты мира — перетаскивание внутри окна карты
/// (левый drag без действия; общий payload как у PopupDrag).
#[derive(Debug)]
pub(crate) struct MapPan;

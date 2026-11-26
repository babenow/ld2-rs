//! M35: ПКМ-осмотр чужого персонажа — всплывающая карточка по клику
//! (не по наведению): уровень, класс, экипировка и профессии.

use gpui::{div, prelude::*, px, rgb, AnyElement, Context, MouseButton};

use ld2_core::l10n::{t, Key};

use crate::palette::Palette;

use super::professions::prof_label;
use super::sheet::{rarity_color, slot_label};
use super::{Root, SendSlot};

/// Порядок слотов, если сервер не прислал `inventory.equip_slots`
/// (старый сервер, §11).
const SLOT_ORDER: [&str; 15] = [
    "head",
    "amulet",
    "cloak",
    "shoulders",
    "body",
    "bracers",
    "gloves",
    "legs",
    "feet",
    "ring1",
    "ring2",
    "main_hand",
    "off_hand",
    "artifact1",
    "artifact2",
];

impl Root {
    /// Закрыть карточку осмотра (клик мимо/Esc/повторный клик).
    pub(crate) fn close_inspect(&mut self, cx: &mut Context<'_, Self>) {
        self.store.inspect = None;
        self.inspect_at = None;
        cx.notify();
    }

    /// Название класса персонажа (warrior/archer/mage) из l10n.
    fn inspect_profile(&self, profile: Option<&str>) -> Option<String> {
        let key = match profile? {
            "warrior" => Key::ProfileWarrior,
            "archer" => Key::ProfileArcher,
            "mage" => Key::ProfileMage,
            _ => return None,
        };
        Some(t(self.lang, key).to_string())
    }

    /// M35: карточка осмотра — появляется по «Осмотреть» (ПКМ), не по
    /// наведению; закрывается кликом мимо/Esc. Позиция — курсор в
    /// момент выбора пункта меню (у краёв окна прижимается внутрь).
    pub(crate) fn inspect_popup(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let data = self.store.inspect.clone()?;
        let lang = self.lang;
        let (vw, vh) = self.viewport;

        // Слоты — порядок конфига (как в окне персонажа).
        let slots: Vec<String> = self
            .store
            .session
            .as_ref()
            .map(|s| s.inventory.equip_slots.clone())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| SLOT_ORDER.iter().map(|s| s.to_string()).collect());

        let mut equip_rows: Vec<AnyElement> = Vec::new();
        for slot in &slots {
            let Some(entry) = data.equip.iter().find(|e| &e.slot == slot) else {
                continue;
            };
            let item = &entry.item;
            let mut name = div()
                .text_color(rgb(rarity_color(&item.rarity, pal)))
                .child(item.name.clone());
            if item.upgrade > 0 {
                name = name.child(format!(" +{}", item.upgrade));
            }
            equip_rows.push(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_color(rgb(pal.muted))
                            .child(slot_label(lang, slot)),
                    )
                    .child(name)
                    .into_any_element(),
            );
        }
        if equip_rows.is_empty() {
            equip_rows.push(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::InspectNoEquipment))
                    .into_any_element(),
            );
        }

        let mut prof_rows: Vec<AnyElement> = data
            .professions
            .iter()
            .map(|p| {
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .gap_2()
                    .child(prof_label(lang, &p.id))
                    .child(div().text_color(rgb(pal.muted)).child(format!(
                        "{} {} · {}",
                        t(lang, Key::InspectRank),
                        p.rank,
                        p.level
                    )))
                    .into_any_element()
            })
            .collect();
        if prof_rows.is_empty() {
            prof_rows.push(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::InspectNoProfessions))
                    .into_any_element(),
            );
        }

        let mut sub = format!("{} {}", t(lang, Key::InspectLevel), data.level);
        if let Some(profile) = self.inspect_profile(data.profile.as_deref()) {
            sub.push_str(" · ");
            sub.push_str(&profile);
        }

        let row_count = equip_rows.len() + prof_rows.len();
        let est_h = 96.0 + row_count as f32 * 20.0;
        let width = 340.0f32;
        let max_h = (vh - 24.0).max(240.0);
        let height = est_h.min(max_h);
        let (mx, my) = self.inspect_at.unwrap_or((vw * 0.5, vh * 0.5));
        let x = mx.min((vw - width - 12.0).max(12.0)).max(12.0);
        let y = if my + height + 16.0 > vh {
            (my - height - 8.0).max(12.0)
        } else {
            my
        };

        let card = div()
            .id("ld2-inspect-card")
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(width))
            .max_h(px(max_h))
            .overflow_y_scroll()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(pal.border))
            .shadow(vec![gpui::BoxShadow {
                color: gpui::rgba((pal.text << 8) | 0x2a).into(),
                offset: gpui::point(px(0.0), px(6.0)),
                blur_radius: px(18.0),
                spread_radius: px(0.0),
                inset: false,
            }])
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().font_weight(gpui::FontWeight::BOLD).child(format!(
                "{}: {}",
                t(lang, Key::InspectTitle),
                data.name
            )))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(sub),
            )
            .child(div().h(px(1.0)).my_1().bg(rgb(pal.border)))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.0))
                    .child(t(lang, Key::InspectEquipment)),
            )
            .children(equip_rows)
            .child(div().h(px(1.0)).my_1().bg(rgb(pal.border)))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.0))
                    .child(t(lang, Key::InspectProfessions)),
            )
            .children(prof_rows);

        Some(
            div()
                .id("ld2-inspect-layer")
                .absolute()
                .inset_0()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.close_inspect(cx)),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _, _, cx| this.close_inspect(cx)),
                )
                .child(card)
                .into_any_element(),
        )
    }

    /// Повторный клик по строке игрока закрывает уже открытую карточку.
    pub(crate) fn toggle_inspect(&mut self, char_id: &str, cx: &mut Context<'_, Self>) {
        if self.store.inspect.as_ref().is_some_and(|c| c.id == char_id) {
            self.close_inspect(cx);
            return;
        }
        let cmd = ld2_protocol::cmd::CharInspect {
            char_id: char_id.to_string(),
        };
        if self.send_or_notice(&cmd, SendSlot::World) {
            self.store.on_world_sent(ld2_core::store::Act::Inspect);
        }
    }
}

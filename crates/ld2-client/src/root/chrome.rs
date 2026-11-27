//! Хром окна: заголовочная полоса (свои кнопки окна), шапка с вкладками
//! и статус-бар логина/выбора персонажа. Вынесено из `root.rs` (RF3g).

use super::*;

const SECONDS_PER_DAY: i64 = 86_400;

/// M33: остаток премиума в днях (округление вверх; истёк → `None`).
pub(crate) fn premium_days_left(until: i64, now: i64) -> Option<i64> {
    let left = until - now;
    (left > 0).then(|| (left + SECONDS_PER_DAY - 1) / SECONDS_PER_DAY)
}

/// M33: «12 дней» с русскими формами (1 день / 2–4 дня / 5+ дней);
/// английский — day/days.
pub(crate) fn premium_days_text(lang: Lang, days: i64) -> String {
    let key = match lang {
        Lang::Ru => match (days % 10, days % 100) {
            (1, n) if n != 11 => Key::PremiumDayOne,
            (2..=4, n) if !(12..=14).contains(&n) => Key::PremiumDayFew,
            _ => Key::PremiumDayMany,
        },
        Lang::En => {
            if days == 1 {
                Key::PremiumDayOne
            } else {
                Key::PremiumDayMany
            }
        }
    };
    format!("{days} {}", t(lang, key))
}

impl Root {
    /// M14/F15: **TitleBar** — своя верхняя строка окна (системный
    /// скрыт): слева название приложения внутри drag-региона (тянем
    /// окно через `Window::start_window_move`), справа — настройки и
    /// WindowControl ✕. На macOS слева резерв под «светофор».
    pub(super) fn titlebar(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let settings_open = self.settings_open;
        let in_world = self.store.screen == Screen::Game;
        let before_login = self.store.screen != Screen::Login;
        let left_pad = if matches!(host_os(), gpui::OperatingSystem::Mac) {
            76.0
        } else {
            12.0
        };
        // M33: премиум-аккаунт — золотым в title bar с остатком в днях.
        let premium_label = self
            .store
            .session
            .as_ref()
            .and_then(|s| s.premium_until)
            .and_then(|until| premium_days_left(until, local_epoch() + self.store.time_offset))
            .map(|days| {
                div()
                    .id("ld2-titlebar-premium")
                    .ml_2()
                    .px_1()
                    .rounded(px(4.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(rgb(pal.gold))
                    .tooltip(Tooltip::text(t(lang, Key::PremiumTooltip)))
                    .child(format!(
                        "{} ({})",
                        t(lang, Key::PremiumLabel),
                        premium_days_text(lang, days)
                    ))
            });
        div()
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .py_1()
            .pl(px(left_pad))
            .bg(rgb(pal.panel))
            .border_b_1()
            .border_color(rgb(pal.border))
            .child(
                div()
                    .id("ld2-titlebar-drag")
                    .flex()
                    .flex_grow_1()
                    .items_center()
                    .cursor_pointer()
                    // M14/F23: нативный hit-test caption — на Windows
                    // окно тянется через HTCAPTION (start_window_move
                    // там — no-op трейта); на macOS этот метод —
                    // заглушка, работает on_drag ниже.
                    .window_control_area(WindowControlArea::Drag)
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(t(lang, Key::AppTitle)),
                    )
                    // M33: премиум — золотым рядом с названием.
                    .children(premium_label)
                    // Drag-регион: кнопки лежат вне его и не мешают.
                    .on_drag(TitlebarDrag, |_value, _offset, window, acx| {
                        window.start_window_move();
                        acx.new(|_| EmptyView)
                    }),
            )
            // M24: админ-панель — только role=admin и в мире
            // (сервер гейтит повторно).
            .when(
                in_world
                    && self
                        .store
                        .session
                        .as_ref()
                        .is_some_and(|s| s.role == "admin"),
                |d| {
                    d.child(
                        IconButton::new("ld2-titlebar-admin", IconName::Shield)
                            .tooltip(t(lang, Key::AdminPanelTitle))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.on_click(Click::AdminOpen, cx);
                                cx.notify();
                            })),
                    )
                },
            )
            // M26: карта мира (заказчик: кнопка в шапке рядом с
            // настройками; только в мире — карта привязана к игре).
            .when(in_world, |d| {
                d.child(
                    IconButton::new("ld2-titlebar-map", IconName::Map)
                        .tooltip(t(lang, Key::MapBtn))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.map_popup.open {
                                this.map_popup.open = false;
                            } else {
                                this.open_popup(PopupKind::Map);
                            }
                            cx.notify();
                        })),
                )
            })
            // Настройки (перенесены из шапки).
            .child(
                div()
                    .when(settings_open, |d| d.rounded_md().bg(rgb(pal.accent)))
                    .child(
                        IconButton::new("ld2-titlebar-settings", IconName::Settings)
                            .tooltip(t(lang, Key::SettingsBtn))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.on_click(Click::ToggleSettings, cx);
                                cx.notify();
                            })),
                    ),
            )
            // M14/F16: кнопки выхода вместо меню шапки — смена
            // персонажа (в мире) и дисконнект (везде, кроме входа).
            .when(in_world, |d| {
                d.child(
                    IconButton::new("ld2-titlebar-switch", IconName::Users)
                        .tooltip(t(lang, Key::BtnSwitchChar))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.on_click(Click::SwitchChar, cx);
                            cx.notify();
                        })),
                )
            })
            .when(!before_login, |d| {
                d.child(
                    IconButton::new("ld2-titlebar-logout", IconName::LogOut)
                        .tooltip(t(lang, Key::BtnLogout))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.on_click(Click::Logout, cx);
                            cx.notify();
                        })),
                )
            })
            // M14/F23 (Windows): системные кнопки окна _ □ ✕ — ely
            // рисует их в стиле платформы (caption-кнопки, hover даёт
            // snap-макеты Win11). На macOS свои не нужны: системные
            // «светофоры» видны сквозь прозрачный тайтлбар.
            .when(!matches!(host_os(), gpui::OperatingSystem::Mac), |d| {
                d.child(WindowControls::new("ld2-titlebar-controls"))
            })
    }

    pub(super) fn header(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let update = self.update_available.clone();
        let lang = self.lang;
        // M14/F1: статус сервера и текущая локация — прямо в шапке.
        let location = self
            .store
            .snapshot
            .as_ref()
            .map(|s| (s.location.name.clone(), s.location.flags.clone()));

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .bg(rgb(pal.panel))
            .border_b_1()
            .border_color(rgb(pal.border))
            .child(
                // Левая группа: (в мире) локация и бейджи — название
                // приложения уехало в TitleBar (M14/F15).
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .children(location.map(|(name, flags)| {
                        // M14/F15: название локации — зелёное, если
                        // безопасная, красное, если небезопасная (pvp).
                        let pvp = flags.iter().any(|f| f.eq_ignore_ascii_case("pvp"));
                        let name_color = if pvp { pal.danger } else { pal.success };
                        // M15/F25: локация в 2 строки — 1) название,
                        // 2) тип локации (бейджи Safe/PvP…).
                        let name_row = div().flex().flex_row().items_center().gap_1().child(
                            div()
                                .text_size(px(crate::ui_tokens::FONT_SM))
                                .text_color(rgb(name_color))
                                .child(format!("· {name}")),
                        );
                        let mut badges_row = div().flex().flex_row().items_center().gap_1();
                        for (tone, token) in location_badges(&flags) {
                            let color = match tone {
                                BadgeTone::Warn => pal.warn,
                                BadgeTone::Success => pal.success,
                                BadgeTone::Muted => pal.muted,
                            };
                            badges_row =
                                badges_row.child(badge(badge_label(lang, &token), color, pal));
                        }
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(name_row)
                            .child(badges_row)
                    }))
                    // M15/F25: разделитель и кнопка окна персонажа рядом
                    // с локацией (только где есть снапшот — в мире).
                    .children(
                        self.store
                            .snapshot
                            .as_ref()
                            .map(|_| div().w(px(1.0)).h(px(20.0)).bg(rgb(pal.border))),
                    )
                    .children(self.store.snapshot.as_ref().map(|_| {
                        btn(
                            cx,
                            t(lang, Key::BtnCharacter),
                            pal.panel,
                            pal.text,
                            Click::ToggleCharacter,
                        )
                    }))
                    // M16/F29b: кнопка рюкзака рядом с «Персонаж».
                    .children(self.store.snapshot.as_ref().map(|_| {
                        btn(
                            cx,
                            t(lang, Key::SheetInventory),
                            pal.panel,
                            pal.text,
                            Click::ToggleInventory,
                        )
                    }))
                    // M18/F36: кнопка квестов рядом с «Рюкзак».
                    .children(self.store.snapshot.as_ref().map(|_| {
                        btn(
                            cx,
                            t(lang, Key::QuestsTab),
                            pal.panel,
                            pal.text,
                            Click::ToggleQuests,
                        )
                    }))
                    // M21: кнопка профессий рядом с «Квесты».
                    .children(self.store.snapshot.as_ref().map(|_| {
                        btn(
                            cx,
                            t(lang, Key::ProfessionsTab),
                            pal.panel,
                            pal.text,
                            Click::ToggleProfessions,
                        )
                    })),
            )
            .child(
                // Правая группа: обновление, язык, настройки и меню
                // шапки (M14/F1). Статус сервера — в статус-баре игры
                // (заказчик 2026-11-20); отметка роли убрана совсем.
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .children(update.map(|v| {
                        btn(
                            cx,
                            tf(lang, Key::UpdateBadge, &[("{v}", &format!("v{v}"))]),
                            pal.success,
                            pal.accent_text,
                            Click::OpenReleases,
                        )
                    })),
            )
    }

    /// Статус-бар экранов логина/выбора персонажа (заказчик): в центре —
    /// автор и версия клиента, справа — статус сервера.
    #[allow(unused_variables)] // cx — паритет сигнатуры статус-баров
    pub(super) fn meta_status_bar(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let status = self.store.transport.clone();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(rgb(pal.border))
            .flex_shrink_0()
            .child(div().flex_grow_1())
            .child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_SM))
                    .text_color(rgb(pal.muted))
                    .child(tf(
                        lang,
                        Key::StatusBarAuthor,
                        &[("{author}", AUTHOR), ("{v}", env!("CARGO_PKG_VERSION"))],
                    )),
            )
            .child(div().flex_grow_1())
            .child(status_badge(pal, lang, &status, self.store.rtt_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M33: остаток премиума — округление вверх, истёкший скрыт.
    #[test]
    fn premium_days_left_rounds_up_and_expires() {
        assert_eq!(premium_days_left(1_000, 999), Some(1));
        assert_eq!(premium_days_left(86_400, 0), Some(1));
        assert_eq!(premium_days_left(86_401, 0), Some(2));
        assert_eq!(premium_days_left(100, 100), None, "истёк");
        assert_eq!(premium_days_left(99, 100), None, "просрочен");
    }

    /// M33: формы «день/дня/дней» (1, 2–4, 5+, 11, 21).
    #[test]
    fn premium_days_text_plural() {
        let ru = Lang::Ru;
        assert_eq!(premium_days_text(ru, 1), "1 день");
        assert_eq!(premium_days_text(ru, 2), "2 дня");
        assert_eq!(premium_days_text(ru, 4), "4 дня");
        assert_eq!(premium_days_text(ru, 5), "5 дней");
        assert_eq!(premium_days_text(ru, 11), "11 дней");
        assert_eq!(premium_days_text(ru, 21), "21 день");
        assert_eq!(premium_days_text(ru, 22), "22 дня");
        assert_eq!(premium_days_text(Lang::En, 1), "1 day");
        assert_eq!(premium_days_text(Lang::En, 3), "3 days");
    }
}

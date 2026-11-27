use ld2_core::domain;

use super::*;

/// M14/F9: вкладки экрана настроек (состояние сессии, конфиг не трогаем).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    /// Тема, язык, шрифты, история чата, уведомления, путь конфига.
    General,
    /// Шрифты: глобальный масштаб, шрифт чата и лога (M14/F14).
    Fonts,
    /// Адрес сервера: read-only + «подключиться к другому» (M14/F11).
    Connection,
    /// Ремаппинг 11 действий (навигация/цикл + боевые 1–6).
    Hotkeys,
}

impl Root {
    /// Окно настроек (ФР-19) — как отдельный экран с кнопкой закрытия.
    /// M14/F14: тело настроек для диалога `settings_dialog` — без
    /// заголовка и кнопки «Закрыть» (их даёт Dialog: крестик/Esc/скрим).
    fn settings_content(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        // Размер задаёт карточка settings_layer (760×560); здесь —
        // растяжение на остаток, чтобы тело скроллилось внутри.
        let mut root = div().flex().flex_col().gap_3().flex_grow_1().min_h_0();
        let active = match self.settings_tab {
            SettingsTab::General => domain::BAG_KIND_GENERAL,
            SettingsTab::Fonts => "fonts",
            SettingsTab::Connection => "connection",
            SettingsTab::Hotkeys => "hotkeys",
        };
        // M14/F12: иконки вкладок (метки без изменений).
        let choices = vec![
            Choice::new(domain::BAG_KIND_GENERAL, t(self.lang, Key::SettingsGeneral))
                .icon(IconName::SlidersHorizontal),
            Choice::new("fonts", t(self.lang, Key::SettingsFonts)).icon(IconName::Type),
            Choice::new("connection", t(self.lang, Key::SettingsConnection)).icon(IconName::Server),
            Choice::new("hotkeys", t(self.lang, Key::SettingsHotkeys)).icon(IconName::Keyboard),
        ];
        let settings_this = cx.entity();
        root = root.child(Tabs::new("ld2-settings-tabs", choices, active).on_change(
            move |value, _window, acx| {
                let value = value.to_string();
                settings_this.update(acx, |root, cx| {
                    root.settings_tab = match value.as_str() {
                        "fonts" => SettingsTab::Fonts,
                        "connection" => SettingsTab::Connection,
                        "hotkeys" => SettingsTab::Hotkeys,
                        _ => SettingsTab::General,
                    };
                    // Прокрутка новой вкладки — с начала.
                    root.settings_scroll.scroll_to_item(0);
                    cx.notify();
                });
            },
        ));
        // M14/F9: тело — только содержимое активной вкладки.
        let body = match self.settings_tab {
            SettingsTab::General => self.settings_tab_general(cx, pal),
            SettingsTab::Fonts => self.settings_tab_fonts(cx, pal),
            SettingsTab::Connection => self.settings_tab_connection(cx, pal),
            SettingsTab::Hotkeys => self.settings_tab_hotkeys(cx, pal),
        };
        // M14/F9: тело активной вкладки — в скролле; заголовок, вкладки
        // и кнопка «Закрыть» остаются вне прокрутки.
        root.child(scrollable_tab(
            "ld2-settings-scroll",
            &self.settings_scroll,
            body,
        ))
    }

    /// Вкладка «Основные»: тема, язык, цвета и жирность журнала, лимит
    /// истории чата, уведомления, мат-фильтр, путь конфига.
    fn settings_tab_general(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let theme = self.config.theme.clone();
        let limit = self.config.chat_limit;
        let notify = self.config.notify;
        let mut body = div().flex().flex_col().gap_3();

        // Тема.
        body = body.child(row(
            pal,
            t(lang, Key::SettingsTheme).to_string(),
            vec![
                btn(
                    cx,
                    t(lang, Key::ThemeDark).to_string(),
                    if theme == domain::THEME_DARK {
                        pal.accent
                    } else {
                        pal.panel
                    },
                    pal.text,
                    Click::Theme(domain::THEME_DARK),
                ),
                btn(
                    cx,
                    t(lang, Key::ThemeLight).to_string(),
                    if theme == "light" {
                        pal.accent
                    } else {
                        pal.panel
                    },
                    pal.text,
                    Click::Theme("light"),
                ),
            ],
        ));

        // M14/F8: язык (перенесён из шапки) — зеркало строки «Тема».
        body = body.child(row(
            pal,
            t(lang, Key::Language).to_string(),
            vec![
                btn(
                    cx,
                    "RU",
                    if lang.code() == "ru" {
                        pal.accent
                    } else {
                        pal.panel
                    },
                    pal.text,
                    Click::Lang("ru"),
                ),
                btn(
                    cx,
                    "EN",
                    if lang.code() == "en" {
                        pal.accent
                    } else {
                        pal.panel
                    },
                    pal.text,
                    Click::Lang("en"),
                ),
            ],
        ));

        // M14/F13: стиль строк боя в журнале: цвета — ColorPicker,
        // жирность урона — Вкл/Выкл (зеркало «Уведомлений»).
        body = body.child(color_picker_row(
            cx,
            pal,
            lang,
            ColorRow {
                label: Key::SettingsLogDamageColor,
                id: "ld2-log-damage-color",
                value: &self.config.log_damage_color,
                open: self.color_picker_open == Some("ld2-log-damage-color"),
                set: |root, v| root.config.log_damage_color = v,
            },
        ));
        body = body.child(color_picker_row(
            cx,
            pal,
            lang,
            ColorRow {
                label: Key::SettingsLogAttackColor,
                id: "ld2-log-attack-color",
                value: &self.config.log_attack_color,
                open: self.color_picker_open == Some("ld2-log-attack-color"),
                set: |root, v| root.config.log_attack_color = v,
            },
        ));
        // M14/F14: флаг — слайдер-переключатель (Ely Switch).
        body = body.child(row(
            pal,
            t(lang, Key::SettingsDamageBold).to_string(),
            vec![div().child({
                let this = cx.entity();
                Switch::new("ld2-log-damage-bold", self.config.log_damage_bold).on_change(
                    move |on, _window, acx| {
                        this.update(acx, |root, cx| {
                            root.on_click(Click::LogDamageBold(on), cx);
                            cx.notify();
                        });
                    },
                )
            })],
        ));

        // Лимит истории чата.
        body = body.child(row(
            pal,
            t(lang, Key::SettingsChatLimit).to_string(),
            vec![
                btn(cx, "−500", pal.panel, pal.text, Click::ChatLimit(-500)),
                div().px_2().child(format!("{limit}")),
                btn(cx, "+500", pal.panel, pal.text, Click::ChatLimit(500)),
            ],
        ));

        // Уведомления (D7).
        // M14/F14: флаг — слайдер-переключатель (Ely Switch).
        body = body.child(row(
            pal,
            t(lang, Key::SettingsNotify).to_string(),
            vec![div().child({
                let this = cx.entity();
                Switch::new("ld2-notify", notify).on_change(move |on, _window, acx| {
                    this.update(acx, |root, cx| {
                        root.on_click(Click::SetNotify(on), cx);
                        cx.notify();
                    });
                })
            })],
        ));

        // Мат-фильтр исходящих (заказчик 2026-11-20).
        body = body.child(row(
            pal,
            t(lang, Key::MuteFilterLabel).to_string(),
            vec![div().child({
                let this = cx.entity();
                Switch::new("ld2-mute-filter", self.config.chat_mute_filter).on_change(
                    move |on, _window, acx| {
                        this.update(acx, |root, cx| {
                            root.on_click(Click::SetMuteFilter(on), cx);
                            cx.notify();
                        });
                    },
                )
            })],
        ));

        // M28: рюкзак показывает все надетые сумки (по умолчанию).
        body = body.child(row(
            pal,
            t(lang, Key::SettingsShowAllBags).to_string(),
            vec![div().child({
                let this = cx.entity();
                Switch::new("ld2-show-all-bags", self.config.backpack_show_all_bags).on_change(
                    move |on, _window, acx| {
                        this.update(acx, |root, cx| {
                            root.on_click(Click::SetBackpackShowAllBags(on), cx);
                            cx.notify();
                        });
                    },
                )
            })],
        ));

        // Путь конфига (ФР-19: импорт/экспорт — правка файла вручную).
        if let Ok(dirs) = ld2_core::config::project_dirs() {
            body = body.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(pal.muted))
                    .child(dirs.config_dir().display().to_string()),
            );
        }

        body
    }

    /// Вкладка «Шрифты»: глобальный масштаб, шрифт чата и строк лога.
    fn settings_tab_fonts(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let mut body = div().flex().flex_col().gap_3();
        // M11.5: размер шрифта (НФР: 1.0…1.4, min 12px) — Slider Ely
        // пишет Theme.font_scale (отклонение M10 «font_scale не
        // применяется» снято).
        body = body.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .w(px(130.0))
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::SettingsFont)),
                )
                .child(slider_cell({
                    let this = cx.entity();
                    let value = self.config.font_scale as f64;
                    Slider::new("ld2-font-scale", value)
                        .range(1.0, 1.4)
                        .step(0.05)
                        .on_change(move |v, _window, acx| {
                            this.update(acx, |root, cx| {
                                root.config.font_scale = v as f32;
                                cx.global_mut::<Theme>().font_scale = v as f32;
                                cx.refresh_windows();
                                cx.notify();
                                root.save_config();
                            });
                        })
                }))
                .child(value_readout(pal, format!("{:.2}", self.config.font_scale))),
        );

        // M14/F8: шрифт сообщений чата и строк лога — отдельные px.
        body = body.child(font_px_row(
            cx,
            pal,
            lang,
            Key::SettingsChatFont,
            "ld2-chat-font-px",
            self.config.chat_font_px,
            |root, v| root.config.chat_font_px = v,
        ));
        body = body.child(font_px_row(
            cx,
            pal,
            lang,
            Key::SettingsLogFont,
            "ld2-log-font-px",
            self.config.log_font_px,
            |root, v| root.config.log_font_px = v,
        ));
        body
    }

    /// Вкладка «Соединение»: адрес сервера (read-only) и режим discovery,
    /// поля ввода раскрываются действием «Подключиться к другому».
    fn settings_tab_connection(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let host_ok = host_allowed(&self.config.host, RELEASE);
        let mut body = div().flex().flex_col().gap_3();
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::SettingsHost)),
                )
                .child(div().text_size(px(14.0)).child(self.config.host.clone()))
                .children((!host_ok).then(|| {
                    div()
                        .text_color(rgb(pal.danger))
                        .child(t(lang, Key::SettingsWsDenied))
                })),
        );
        // M19 (§15): режим discovery — домен (пусто = прямой URL).
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::SettingsDomain)),
                )
                .child(
                    div()
                        .text_size(px(14.0))
                        .child(self.config.domain.clone().unwrap_or_else(|| "—".into())),
                ),
        );
        if self.host_editing {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(t(lang, Key::SettingsHost)),
                    )
                    .child(
                        div()
                            .w(px(320.0))
                            .child(Input::new(self.ti(&self.host_input))),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(t(lang, Key::SettingsDomain)),
                    )
                    .child(
                        div()
                            .w(px(320.0))
                            .child(Input::new(self.ti(&self.domain_input))),
                    )
                    .child(btn(
                        cx,
                        t(lang, Key::SettingsApply).to_string(),
                        pal.accent,
                        pal.accent_text,
                        Click::ApplyHost,
                    )),
            );
        } else {
            body = body.child(btn(
                cx,
                t(lang, Key::SettingsChangeHost).to_string(),
                pal.panel,
                pal.text,
                Click::EditHost,
            ));
        }
        body
    }

    /// Вкладка «Горячие клавиши»: только отображение (16 действий).
    fn settings_tab_hotkeys(&mut self, cx: &mut Context<'_, Self>, _pal: &Palette) -> Div {
        let lang = self.lang;
        let mut body = div().flex().flex_col().gap_3();
        // Горячие клавиши: только отображение (редактирование/применение
        // навигации — M10.4 вместе с game-навигацией).
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(t(lang, Key::SettingsHotkeys)),
                )
                // M14/F4: все ремаппингуемые действия — из одного
                // вектора (навигация/цикл + боевые 1–6), без match
                // по индексу.
                .children(HOTKEY_ROWS.iter().map(|row| {
                    let value = (row.get)(&self.config.hotkeys).to_string();
                    hotkey_row(cx, lang, row, &value)
                })),
        );

        body
    }

    /// M14/F15: настройки — снова Ely `Dialog` (решение пользователя):
    /// Esc/скрим/крестик закрывают, карточка центрируется с анимацией;
    /// тело карточки скроллится самим Dialog'ом (440 px — контент
    /// адаптируется, фикс-размер больше не нужен).
    pub(super) fn settings_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if !self.settings_open {
            return None;
        }
        let title = t(self.lang, Key::SettingsTitle).to_string();
        let close = self.dismiss(cx, |root| root.close_settings());
        Some(
            Dialog::new("ld2-settings", title, close)
                .child(self.settings_content(cx, pal))
                .into_any_element(),
        )
    }

    /// M14/F14: свотч открывает/закрывает пикер этого цвета (один разом).
    pub(super) fn toggle_color_picker(&mut self, key: &'static str) {
        self.color_picker_open = match self.color_picker_open {
            Some(open) if open == key => None,
            _ => Some(key),
        };
    }
}

/// Строка хоткея: `HotkeyInput` Ely — запись нажатия и очистка Backspace
/// (ФР-19). Приоритет над навигацией даёт сам виджет (фокус-режим),
/// ручная capture-машинка M10 упразднена; значение хранится в config.toml
/// форматом `alt+up` (round-trip через Keystroke::parse_with_os).
/// M14/F4: строка настройки хоткея — id виджета, ключ l10n и доступ к
/// спецификации в конфиге (геттер/сеттер вместо match по индексу).
pub(super) struct HotkeyRowSpec {
    pub(super) id: &'static str,
    pub(super) label: Key,
    pub(super) get: fn(&Hotkeys) -> &str,
    pub(super) set: fn(&mut Hotkeys, String),
}

/// Все ремаппингуемые действия: навигация/цикл (ФР-19) + боевые
/// панели (M14/F4, цифры 1–6 — Esc занят диалогами).
pub(super) const HOTKEY_ROWS: [HotkeyRowSpec; 16] = [
    HotkeyRowSpec {
        id: "ld2-hk-nav-up",
        label: Key::HkNavUp,
        get: |hk| hk.nav_up.as_str(),
        set: |hk, v| hk.nav_up = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-nav-down",
        label: Key::HkNavDown,
        get: |hk| hk.nav_down.as_str(),
        set: |hk, v| hk.nav_down = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-nav-left",
        label: Key::HkNavLeft,
        get: |hk| hk.nav_left.as_str(),
        set: |hk, v| hk.nav_left = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-nav-right",
        label: Key::HkNavRight,
        get: |hk| hk.nav_right.as_str(),
        set: |hk, v| hk.nav_right = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-cycle",
        label: Key::HkCycleTarget,
        get: |hk| hk.cycle_target.as_str(),
        set: |hk, v| hk.cycle_target = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-attack",
        label: Key::Attack,
        get: |hk| hk.action_attack.as_str(),
        set: |hk, v| hk.action_attack = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-power-strike",
        label: Key::PowerStrike,
        get: |hk| hk.action_power_strike.as_str(),
        set: |hk, v| hk.action_power_strike = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-cleave",
        label: Key::HkCleave,
        get: |hk| hk.action_cleave.as_str(),
        set: |hk, v| hk.action_cleave = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-flee",
        label: Key::BtnFlee,
        get: |hk| hk.action_flee.as_str(),
        set: |hk, v| hk.action_flee = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-rune",
        label: Key::HkRune,
        get: |hk| hk.action_rune.as_str(),
        set: |hk, v| hk.action_rune = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-consumable",
        label: Key::HkConsumable,
        get: |hk| hk.action_consumable.as_str(),
        set: |hk, v| hk.action_consumable = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-character",
        label: Key::HkOpenCharacter,
        get: |hk| hk.open_character.as_str(),
        set: |hk, v| hk.open_character = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-backpack",
        label: Key::HkOpenBackpack,
        get: |hk| hk.open_backpack.as_str(),
        set: |hk, v| hk.open_backpack = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-quests",
        label: Key::HkOpenQuests,
        get: |hk| hk.open_quests.as_str(),
        set: |hk, v| hk.open_quests = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-professions",
        label: Key::HkOpenProfessions,
        get: |hk| hk.open_professions.as_str(),
        set: |hk, v| hk.open_professions = v,
    },
    HotkeyRowSpec {
        id: "ld2-hk-map",
        label: Key::HkOpenMap,
        get: |hk| hk.open_map.as_str(),
        set: |hk, v| hk.open_map = v,
    },
];

fn hotkey_row(cx: &mut Context<'_, Root>, lang: Lang, row: &HotkeyRowSpec, spec: &str) -> Div {
    let want = gpui::Keystroke::parse_with_os(&spec.replace('+', "-"), host_os()).ok();
    let this = cx.entity();
    let set = row.set;
    div()
        .flex()
        .flex_row()
        .gap_2()
        .items_center()
        .text_size(px(13.0))
        .child(
            div()
                .flex_grow_1()
                .text_color(rgb(0x7f848e))
                .child(t(lang, row.label)),
        )
        .child(
            HotkeyInput::new(row.id, want).on_change(move |ks, _window, acx| {
                let spec = ks
                    .as_ref()
                    .and_then(spec_from_keystroke)
                    .unwrap_or_default();
                this.update(acx, |root, cx| {
                    set(&mut root.config.hotkeys, spec);
                    root.save_config();
                    cx.notify();
                });
            }),
        )
}

/// M14/F8: строка настройки «размер шрифта, px» — Slider 10…20 с шагом 1;
/// пишет поле конфига и перерисовывает окно (значение — абсолютные px).
fn font_px_row(
    cx: &mut Context<'_, Root>,
    pal: &Palette,
    lang: Lang,
    label: Key,
    id: &'static str,
    value: f32,
    set: fn(&mut Root, f32),
) -> Div {
    let this = cx.entity();
    div()
        .flex()
        .flex_row()
        .gap_2()
        .items_center()
        .child(
            div()
                .w(px(130.0))
                .text_color(rgb(pal.muted))
                .child(t(lang, label)),
        )
        .child(slider_cell(
            Slider::new(id, f64::from(value))
                .range(10.0, 20.0)
                .step(1.0)
                .on_change(move |v, _window, acx| {
                    this.update(acx, |root, cx| {
                        set(root, v as f32);
                        cx.refresh_windows();
                        cx.notify();
                        root.save_config();
                    });
                }),
        ))
        .child(value_readout(pal, format!("{} px", value.round() as i32)))
}

/// M14/F13: hex из настроек → RGB (парсер Ely — те же формы, что
/// принимает `is_hex_color` в sanitize конфига); битый → `None`
/// (вызывающий берёт дефолт). В F14 перенесено сюда из `game.rs` —
/// нужно и для свотча в настройках.
pub(crate) fn hex_to_u32(text: &str) -> Option<u32> {
    let rgba = parse_hex(text).ok()?;
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    Some((byte(rgba.r) << 16) | (byte(rgba.g) << 8) | byte(rgba.b))
}

/// M14/F14: строка настройки цвета — подпись, **свотч-кнопка** (по
/// нажатию открывается/закрывается ColorPicker под строкой) и hex.
/// во всю ширину (он инлайн-блок: превью + плоскость + рейки + hex-поля;
/// тело настроек скроллится — F9). Значение в конфиге — hex; запись —
/// `hex(Hsla::to_rgb)`, чтение — `parse_hex` (те же формы, что
/// принимает `is_hex_color` в sanitize).
/// M14/F14: параметры строки цвета (пакет — иначе clippy
/// `too_many_arguments` при 8 аргументах).
struct ColorRow<'a> {
    label: Key,
    id: &'static str,
    value: &'a str,
    open: bool,
    set: fn(&mut Root, String),
}

fn color_picker_row(
    cx: &mut Context<'_, Root>,
    pal: &Palette,
    lang: Lang,
    spec: ColorRow<'_>,
) -> Div {
    let ColorRow {
        label,
        id,
        value,
        open,
        set,
    } = spec;
    let this = cx.entity();
    // sanitize гарантирует валидный hex; запасной — серый, без паники.
    let rgba = parse_hex(value).unwrap_or_else(|_| parse_hex("#808080").expect("запасной цвет"));
    let hsla = Hsla::from(rgba);
    let swatch = hex_to_u32(value).unwrap_or(0x80_80_80);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(13.0))
                .text_color(rgb(pal.muted))
                .child(t(lang, label)),
        )
        .child(
            // M14/F14: свотч-кнопка — пикер открывается по нажатию.
            div()
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .id(format!("ld2-swatch-{id}"))
                        .cursor_pointer()
                        .w(px(44.0))
                        .h(px(24.0))
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(pal.border))
                        .bg(rgb(swatch))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener({
                                let key = id;
                                move |root, _, _, cx| {
                                    root.toggle_color_picker(key);
                                    cx.notify();
                                }
                            }),
                        ),
                )
                .child(
                    div()
                        .flex_grow_1()
                        .text_size(px(13.0))
                        .text_color(rgb(pal.muted))
                        .child(value.to_string()),
                )
                .when(open, |row| {
                    row.child(btn(
                        cx,
                        t(lang, Key::BtnClose).to_string(),
                        pal.panel,
                        pal.text,
                        Click::TogglePicker(id),
                    ))
                }),
        )
        .when(open, |row| {
            row.child(
                ColorPicker::new(id, hsla)
                    .opaque()
                    .on_change(move |c, _window, acx| {
                        let hex = hex(c.to_rgb());
                        this.update(acx, |root, cx| {
                            set(root, hex);
                            root.save_config();
                            cx.refresh_windows();
                            cx.notify();
                        });
                    }),
            )
        })
}

/// M14/F8 (фикс смоука): слайдер Ely сам ширины не имеет — `Track`
/// рендерится `flex_none` с одним padding, а все его дети абсолютно
/// позиционированы (внешняя ширина ≈ thumb, ≈0 полезной площади: не
/// виден и не кликается). Обёртка — flex-**колонка**: поперечная ось
/// растягивает слайдер на всю ширину ячейки, а `size_full()` у Ely
/// получает реально определённую область.
fn slider_cell(slider: Slider) -> Div {
    div().flex().flex_col().flex_grow_1().child(slider)
}

/// M14/F8: показ текущего значения настройки справа от ползунка.
fn value_readout(pal: &Palette, text: String) -> Div {
    div()
        .w(px(56.0))
        .text_size(px(13.0))
        .text_color(rgb(pal.text))
        .child(text)
}

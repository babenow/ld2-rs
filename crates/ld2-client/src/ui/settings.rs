//! Окно настроек (ФР-19): тема, шрифт, лимит истории, уведомления, хост,
//! ремаппинг горячих клавиш. Пишет в [`crate::config::Config`] и сохраняет
//! `config.toml` по факту изменения; применение темы/шрифта — в `App::ui`.

use eframe::egui;

use crate::config::host_allowed;
use crate::l10n::{t, Key};
use crate::ui::{hotkeys, UiCtx};

const RELEASE_BUILD: bool = !cfg!(debug_assertions);

/// Ключи локализации действий ремаппинга (порядок = [`crate::config::Hotkeys::entries`]).
fn action_key(action: &str) -> Key {
    match action {
        "nav_up" => Key::HkNavUp,
        "nav_down" => Key::HkNavDown,
        "nav_left" => Key::HkNavLeft,
        "nav_right" => Key::HkNavRight,
        _ => Key::HkCycleTarget,
    }
}

/// Сохранение конфига (ошибка — в лог, не в UI: настройки локальные).
fn persist(ctx: &mut UiCtx<'_>) {
    if let Err(e) = ctx.cfg_store.save(ctx.cfg) {
        tracing::warn!("не удалось сохранить конфиг: {e:#}");
    }
}

pub fn modal(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui) {
    let lang = ctx.lang;
    egui::Window::new(t(lang, Key::SettingsTitle))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            // ── Тема (ФР-19) ──
            ui.horizontal(|ui| {
                ui.label(t(lang, Key::SettingsTheme));
                let mut theme = ctx.cfg.theme.clone();
                egui::ComboBox::from_id_salt("theme")
                    .selected_text(if theme == "light" {
                        t(lang, Key::ThemeLight)
                    } else {
                        t(lang, Key::ThemeDark)
                    })
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(theme == "dark", t(lang, Key::ThemeDark))
                            .clicked()
                        {
                            theme = "dark".into();
                        }
                        if ui
                            .selectable_label(theme == "light", t(lang, Key::ThemeLight))
                            .clicked()
                        {
                            theme = "light".into();
                        }
                    });
                if theme != ctx.cfg.theme {
                    ctx.cfg.theme = theme;
                    persist(ctx);
                }
            });

            // ── Размер шрифта (НФР: min 12px) ──
            ui.horizontal(|ui| {
                ui.label(t(lang, Key::SettingsFont));
                let mut scale = ctx.cfg.font_scale;
                if ui
                    .add(egui::Slider::new(&mut scale, 1.0..=1.4).fixed_decimals(2))
                    .changed()
                {
                    ctx.cfg.font_scale = scale;
                    persist(ctx);
                }
            });

            // ── Лимит истории чата (НФР §8, настраивается) ──
            ui.horizontal(|ui| {
                ui.label(t(lang, Key::SettingsChatLimit));
                let mut limit = ctx.cfg.chat_limit as f32;
                if ui
                    .add(
                        egui::Slider::new(&mut limit, 500.0..=10_000.0)
                            .fixed_decimals(0)
                            .logarithmic(true),
                    )
                    .changed()
                {
                    ctx.cfg.chat_limit = limit as usize;
                    ctx.store.chat_limit = ctx.cfg.chat_limit;
                    persist(ctx);
                }
            });

            // ── OS-уведомления (D7, feature-flag) ──
            ui.horizontal(|ui| {
                let mut notify = ctx.cfg.notify;
                if ui
                    .checkbox(&mut notify, t(lang, Key::SettingsNotify))
                    .changed()
                {
                    ctx.cfg.notify = notify;
                    persist(ctx);
                }
            });

            ui.separator();

            // ── Хост сервера (ФР-19: dev/тесты) + guard release ──
            ui.horizontal(|ui| {
                ui.label(t(lang, Key::SettingsHost));
                if ctx.forms.host_draft.is_empty() {
                    ctx.forms.host_draft = ctx.cfg.host.clone();
                }
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut ctx.forms.host_draft).desired_width(240.0),
                );
                let host_ok = host_allowed(ctx.forms.host_draft.trim(), RELEASE_BUILD);
                if !host_ok {
                    ui.colored_label(
                        egui::Color32::from_rgb(0xE0, 0x6C, 0x75),
                        t(lang, Key::SettingsWsDenied),
                    );
                }
                let apply = ui.add_enabled(
                    host_ok && edit.changed() && ctx.forms.host_draft.trim() != ctx.cfg.host,
                    egui::Button::new(t(lang, Key::SettingsApply)),
                );
                if apply.clicked() {
                    ctx.cfg.host = ctx.forms.host_draft.trim().to_string();
                    persist(ctx);
                    // Переподключение транспорта выполняет App после кадра
                    // (UiCtx не владеет транспортом).
                    ctx.forms.host_apply_pending = true;
                }
            });
            if RELEASE_BUILD && ctx.cfg.host.starts_with("ws://") {
                ui.weak(t(lang, Key::SettingsWsWarn));
            }

            ui.separator();

            // ── Ремаппинг горячих клавиш (ФР-19) ──
            ui.strong(t(lang, Key::SettingsHotkeys));
            // Работаем с клоном: заимствование &mut ctx.cfg конфликтовало бы
            // с чтением ctx.forms внутри цикла.
            let mut hk = ctx.cfg.hotkeys.clone();
            let mut hk_changed = false;
            for (idx, (action, spec)) in hk.entries().into_iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(t(lang, action_key(action)));
                    let capturing = ctx.forms.hotkey_capture == Some(idx);
                    let label = if capturing {
                        t(lang, Key::HkPress).to_string()
                    } else {
                        spec.clone()
                    };
                    if ui.selectable_label(capturing, label).clicked() {
                        ctx.forms.hotkey_capture = Some(idx);
                    }
                    if capturing {
                        // Ловим первое нажатие (не-модификатор) → спецификация.
                        let pressed = ui.input(hotkeys::spec_from_input);
                        if let Some(new_spec) = pressed {
                            *spec = new_spec;
                            ctx.forms.hotkey_capture = None;
                            hk_changed = true;
                        }
                    }
                });
            }
            if hk_changed {
                ctx.cfg.hotkeys = hk;
                persist(ctx);
            }

            ui.separator();
            if let Ok(path) = crate::config::project_dirs() {
                ui.weak(path.config_dir().display().to_string());
            }
        });
}

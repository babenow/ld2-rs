//! Экраны входа и выбора персонажа: поля форм и разметка экрана логина.
//!
//! Вынесено из `root.rs` (RF3b): поведение и разметка не менялись.

use super::*;

impl Root {
    /// M11.3: поля Ely создаются на первом кадре — `TextInput::new` требует
    /// Window. Enter в логине/чате = отправка через InputEvent::Submit
    /// (поллинг take_submit упразднён); пароль — masked (отклонение M10 снято).
    pub(super) fn init_inputs(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let lang = self.lang;
        let last_login = self.config.last_login.clone();
        let host = self.config.host.clone();
        let mk = |window: &mut Window,
                  cx: &mut Context<'_, Self>,
                  placeholder: String,
                  masked: bool,
                  initial: Option<String>| {
            cx.new(|cx| {
                let mut i = TextInput::new(window, cx).placeholder(placeholder);
                if masked {
                    i = i.masked();
                }
                if let Some(text) = initial {
                    i.set_text(text, cx);
                }
                i
            })
        };
        self.login_input = Some(mk(
            window,
            cx,
            t(lang, Key::FieldLogin).to_string(),
            false,
            last_login,
        ));
        self.password_input = Some(mk(
            window,
            cx,
            t(lang, Key::FieldPassword).to_string(),
            true,
            // M16/F29j: при включённой галочке «Запомнить пароль»
            // пароль уже введён (чтение keyring — в main.rs, один
            // раз при старте).
            if self.config.remember_password {
                self.prefill_password.take()
            } else {
                None
            },
        ));
        self.host_input = Some(mk(window, cx, host.clone(), false, Some(host)));
        // M19 (§15): поле домена discovery (пусто — прямой режим).
        let domain = self.config.domain.clone().unwrap_or_default();
        self.domain_input = Some(mk(window, cx, domain.clone(), false, Some(domain)));
        self.guild_input = Some(mk(window, cx, "…".into(), false, None));
        self.create_name = Some(mk(
            window,
            cx,
            t(lang, Key::FieldCharName).to_string(),
            false,
            None,
        ));
        // M13.3: поля регистрации (пароль и подтверждение — masked).
        self.reg_login = Some(mk(
            window,
            cx,
            t(lang, Key::FieldLogin).to_string(),
            false,
            None,
        ));
        self.reg_email = Some(mk(
            window,
            cx,
            t(lang, Key::FieldEmail).to_string(),
            false,
            None,
        ));
        self.reg_password = Some(mk(
            window,
            cx,
            t(lang, Key::FieldPassword).to_string(),
            true,
            None,
        ));
        self.reg_password2 = Some(mk(
            window,
            cx,
            t(lang, Key::FieldPassword2).to_string(),
            true,
            None,
        ));
        self.chat_input = Some(mk(window, cx, String::new(), false, None));
        self.chat_filter = Some(mk(
            window,
            cx,
            t(lang, Key::ChFilter).to_string(),
            false,
            None,
        ));
        self.admin_search = Some(mk(
            window,
            cx,
            t(lang, Key::AdminSearch).to_string(),
            false,
            None,
        ));
        self.admin_inv_search = Some(mk(
            window,
            cx,
            t(lang, Key::AdminInvSearch).to_string(),
            false,
            None,
        ));
        self.admin_player = Some(mk(
            window,
            cx,
            t(lang, Key::AdminPlayerName).to_string(),
            false,
            None,
        ));

        self.delete_name = Some(mk(
            window,
            cx,
            t(lang, Key::DeleteTypeNick).to_string(),
            false,
            None,
        ));
        for slot in [self.login_input.clone(), self.password_input.clone()] {
            let ent = slot.expect("создано выше");
            cx.subscribe(&ent, |root, _field, event: &InputEvent, cx| {
                if *event == InputEvent::Submit && root.store.screen == Screen::Login {
                    root.send_login(cx);
                }
            })
            .detach();
        }
        // Enter в полях регистрации — отправка (как в логине, §ФР-01).
        for slot in [
            self.reg_login.clone(),
            self.reg_email.clone(),
            self.reg_password.clone(),
            self.reg_password2.clone(),
        ] {
            let ent = slot.expect("создано выше");
            cx.subscribe(&ent, |root, _field, event: &InputEvent, cx| {
                if *event == InputEvent::Submit
                    && root.store.screen == Screen::Login
                    && root.register_open
                {
                    root.send_register(cx);
                }
            })
            .detach();
        }
        let chat = self.chat_input.clone().expect("создано выше");
        cx.subscribe(&chat, |root, _field, event: &InputEvent, cx| {
            if *event == InputEvent::Submit && root.store.screen == Screen::Game {
                root.send_chat(cx);
            }
        })
        .detach();
    }

    pub(super) fn login_screen(
        &mut self,
        window: &Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Div {
        let lang = self.lang;
        let error = self.store.login_error.clone();
        // M14/F21: живое чтение состояния на каждый рендер — Caps из
        // поля окна (обновляется каждым ModifiersChanged), раскладка —
        // из TIS-источника ввода.
        let caps_on = window.capslock().on;
        let (layout_id, layout_name) = {
            let layout = cx.keyboard_layout();
            (layout.id().to_string(), layout.name().to_string())
        };
        let ru_layout = is_cyrillic_layout(&layout_id, &layout_name);
        div()
            // M14/F21: смена модификаторов (Caps/Shift/Ctrl) → перерисовка:
            // предупреждения читаются в рендере из window/keyboard_layout.
            .on_modifiers_changed(cx.listener(|_, _, _, cx| cx.notify()))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .size_full()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(22.0))
                    .child(t(lang, Key::LoginTitle)),
            )
            .child(
                div()
                    .w(px(320.0))
                    .child(Input::new(self.ti(&self.login_input))),
            )
            .child(
                div()
                    .w(px(320.0))
                    .child(PasswordInput::new(self.ti(&self.password_input))),
            )
            // M16/F29j: «запомнить пароль» — пароль живёт только в
            // keyring; флаг — в конфиге (сохраняется сразу).
            .child({
                let me = cx.entity();
                let on = self.config.remember_password;
                div().w(px(320.0)).child(
                    Checkbox::new("ld2-login-remember", on)
                        .label(t(lang, Key::RememberPassword))
                        .on_change(move |on, _, acx| {
                            me.update(acx, |root, cx| {
                                root.config.remember_password = on;
                                if let Err(e) = root.config_store.save(&root.config) {
                                    tracing::warn!("не удалось сохранить конфиг: {e:#}");
                                }
                                cx.notify();
                            })
                        }),
                )
            })
            // M14/F21: предупреждения Caps/раскладка — под полями ввода.
            .children(caps_on.then(|| {
                div()
                    .w(px(320.0))
                    .text_size(px(crate::ui_tokens::FONT_SM))
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::WarnCaps))
            }))
            .children(ru_layout.then(|| {
                div()
                    .w(px(320.0))
                    .text_size(px(crate::ui_tokens::FONT_SM))
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::WarnRuLayout))
            }))
            .child(btn(
                cx,
                t(lang, Key::BtnLogin).to_string(),
                pal.accent,
                pal.accent_text,
                Click::Login,
            ))
            .child(btn(
                cx,
                t(lang, Key::BtnRegister).to_string(),
                pal.panel,
                pal.text,
                Click::OpenRegister,
            ))
            .children(error.map(|e| div().text_color(rgb(pal.danger)).child(e)))
            .child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_SM))
                    .text_color(rgb(pal.muted))
                    .child("demo/demo123 · demo2/demo123"),
            )
    }
}

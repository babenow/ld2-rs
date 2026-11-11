//! Диалог регистрации (M13.3, D35, ФР-01): Ely `Dialog` поверх экрана
//! входа. Поля: Логин (фокус), Email, Пароль, Подтверждение; порядок и
//! inline-ошибки — из [`ld2_core::register`] (валидация D34 в ядре, сервер
//! — источник истины по лимитам).

use ely::forms::{Input, PasswordInput};
use ely::overlays::Dialog;
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div, Focusable};
use ld2_core::l10n::{t, tf, Key};
use ld2_core::register::{email_loosely_valid, RegisterError, RegisterField, RegisterForm};
use ld2_core::store::Act;
use ld2_protocol::cmd::AuthRegister;

use crate::palette::Palette;

use super::{act_btn, Root};

impl Root {
    /// Диалог регистрации (M13.3): Esc/скрим закрывают, КРОМЕ отправки
    /// (D35) — пока команда в полёте, закрытие игнорируется.
    pub(crate) fn register_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let title = t(self.lang, Key::RegisterTitle).to_string();
        let body = self.register_view(cx, pal).into_any_element();
        let close = self.dismiss(cx, |r| {
            // D35: отправка не прерывается закрытием.
            if r.store.pending == Some(Act::Register) {
                return;
            }
            r.register_open = false;
            r.register_submitted = false;
            r.store.register_error = None;
        });
        // D35: фокус — на поле «Логин» (основное).
        let focus = self.ti(&self.reg_login).read(cx).focus_handle(cx);
        Some(
            Dialog::new("ld2-register", title, close)
                .child(body)
                .focus_first(focus)
                .into_any_element(),
        )
    }

    /// Отправка регистрации: клиентская валидация (D34) → команда.
    /// Ошибки формы (клиентские и серверные) — в `store.register_error`
    /// и рисуются под соответствующим полем (D35).
    pub(crate) fn send_register(&mut self, cx: &mut Context<'_, Self>) {
        if self.store.pending == Some(Act::Register) {
            return; // повторный сабмит заблокирован (состояние «отправка»)
        }
        let form = RegisterForm {
            login: self.ti(&self.reg_login).read(cx).text().to_string(),
            email: self.ti(&self.reg_email).read(cx).text().to_string(),
            password: self.ti(&self.reg_password).read(cx).text().to_string(),
            confirm: self.ti(&self.reg_password2).read(cx).text().to_string(),
        };
        if let Err(issue) = form.validate() {
            // Клиентская проверка (D34): текст локализуем, сервер не трогаем.
            let message = match issue {
                ld2_core::register::RegisterIssue::Empty(_) => {
                    t(self.lang, Key::ErrRegisterFill).to_string()
                }
                ld2_core::register::RegisterIssue::PasswordMismatch => {
                    t(self.lang, Key::ErrPasswordMismatch).to_string()
                }
                ld2_core::register::RegisterIssue::BadEmail => {
                    t(self.lang, Key::ErrBadEmail).to_string()
                }
            };
            self.store.register_error = Some(RegisterError {
                field: issue.field(),
                message,
                retry_after_s: None,
            });
            return;
        }
        let email = form.normalized_email();
        // M16/F29j: пароль до move в cmd — при включённой галочке
        // PersistSession сохранит его в keyring (регистрация =
        // немедленный вход, ADR-006).
        self.pending_password = Some(form.password.clone());
        let cmd = AuthRegister {
            login: form.login.clone(),
            password: form.password,
            email,
        };
        match self.transport.send(&cmd) {
            Ok(_) => {
                self.store.on_register_sent(&form.login);
                self.register_submitted = true;
            }
            Err(e) => {
                self.store.register_error = Some(RegisterError {
                    field: RegisterField::Form,
                    message: format!("нет связи: {e}"),
                    retry_after_s: None,
                })
            }
        }
    }

    /// Тело диалога: поля с inline-ошибками под своим полем + кнопки.
    fn register_view(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let pending = self.store.pending == Some(Act::Register);

        // Успех (ack ok → pending снят, ошибок нет) — окно закрывается;
        // `session.start` дальше переводит на экран персонажей (D35).
        if self.register_submitted && !pending && self.store.register_error.is_none() {
            self.register_open = false;
            self.register_submitted = false;
        }

        let login = self.ti(&self.reg_login).read(cx).text().to_string();
        let email = self.ti(&self.reg_email).read(cx).text().to_string();
        let password = self.ti(&self.reg_password).read(cx).text().to_string();
        let confirm = self.ti(&self.reg_password2).read(cx).text().to_string();
        let form = RegisterForm {
            login: login.clone(),
            email: email.clone(),
            password: password.clone(),
            confirm: confirm.clone(),
        };
        let valid = form.validate().is_ok();
        let err = self.store.register_error.clone();

        // Текст ошибки под своим полем: серверная/клиентская — по field.
        let error_line = |field: RegisterField| -> Option<String> {
            let e = err.as_ref()?;
            if e.field != field {
                return None;
            }
            // Текст сервера — как есть (§9 fallback); retry_after_s —
            // обратный отсчёт ключом ErrRetryAfter.
            Some(match e.retry_after_s {
                Some(s) => tf(lang, Key::ErrRetryAfter, &[("s", &s.to_string())]),
                None => e.message.clone(),
            })
        };
        let line = |text: String, danger: bool| -> Div {
            div()
                .text_size(px(12.0))
                .text_color(rgb(if danger { pal.danger } else { pal.warn }))
                .child(text)
        };

        let mut col = div().flex().flex_col().gap_1().child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::RegisterSubtitle)),
        );

        col = col.child(
            div()
                .w(px(320.0))
                .child(Input::new(self.ti(&self.reg_login))),
        );
        if let Some(text) = error_line(RegisterField::Login) {
            col = col.child(line(text, true));
        }

        col = col.child(
            div()
                .w(px(320.0))
                .child(Input::new(self.ti(&self.reg_email))),
        );
        // Грубая проверка email — как только заполнен (D34).
        if err.is_none() && !email.is_empty() && !email_loosely_valid(email.trim()) {
            col = col.child(line(t(lang, Key::ErrBadEmail).to_string(), false));
        } else if let Some(text) = error_line(RegisterField::Email) {
            col = col.child(line(text, true));
        }

        col = col.child(
            div()
                .w(px(320.0))
                .child(PasswordInput::new(self.ti(&self.reg_password))),
        );
        if let Some(text) = error_line(RegisterField::Password) {
            col = col.child(line(text, true));
        }

        col = col.child(
            div()
                .w(px(320.0))
                .child(PasswordInput::new(self.ti(&self.reg_password2))),
        );
        if err.is_none() && !password.is_empty() && !confirm.is_empty() && password != confirm {
            col = col.child(line(t(lang, Key::ErrPasswordMismatch).to_string(), false));
        } else if let Some(text) = error_line(RegisterField::Confirm) {
            col = col.child(line(text, true));
        }

        // Общая ошибка (rate-limit/регистрация выключена/сеть).
        if let Some(text) = error_line(RegisterField::Form) {
            col = col.child(line(text, true));
        }

        let mut row = div().flex().flex_row().gap_2().mt(px(6.0));
        row = row.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, _cx| {
                if this.store.pending != Some(Act::Register) {
                    this.register_open = false;
                    this.register_submitted = false;
                    this.store.register_error = None;
                }
            },
        ));
        if valid && !pending {
            row = row.child(act_btn(
                cx,
                t(lang, Key::BtnRegister),
                pal.accent,
                pal.accent_text,
                |this, cx| {
                    // Общий путь: валидация → AuthRegister (D34/D35).
                    this.send_register(cx);
                },
            ));
        } else {
            let label = if pending {
                t(lang, Key::RegisterSending)
            } else {
                t(lang, Key::BtnRegister)
            };
            row = row.child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.border))
                    .text_color(rgb(pal.muted))
                    .child(label),
            );
        }
        col = col.child(row);
        col
    }
}

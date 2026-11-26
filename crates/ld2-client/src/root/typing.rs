//! Реестр полей ввода, чей фокус блокирует хоткеи на экране.
//! Вынесено из `root.rs` (RF3j).

use super::*;

/// M15/F25i: поле ввода, чей фокус блокирует хоткеи на экране:
/// (экран, имя поля для теста, доступ к полю Root).
pub(crate) struct TypingField {
    pub(crate) screen: Screen,
    /// Имя поля — только для теста покрытия (в рендере не читается).
    #[allow(dead_code)]
    pub(crate) name: &'static str,
    pub(crate) get: fn(&Root) -> &Option<Entity<TextInput>>,
}

/// M15/F25i: поля ввода по экранам — источник истины
/// `Root::typing_focused` (тест покрывает все TextInput Root): на
/// экране проверяются только его поля — после смены экрана фокус
/// формально остаётся на неотрисованном поле (пароль на Login →
/// Game), и все хоткеи «залипали».
pub(crate) const TYPING_FIELDS: &[TypingField] = &[
    TypingField {
        screen: Screen::Login,
        name: "login",
        get: |r| &r.login_input,
    },
    TypingField {
        screen: Screen::Login,
        name: "password",
        get: |r| &r.password_input,
    },
    TypingField {
        screen: Screen::Login,
        name: "reg_login",
        get: |r| &r.reg_login,
    },
    TypingField {
        screen: Screen::Login,
        name: "reg_email",
        get: |r| &r.reg_email,
    },
    TypingField {
        screen: Screen::Login,
        name: "reg_password",
        get: |r| &r.reg_password,
    },
    TypingField {
        screen: Screen::Login,
        name: "reg_password2",
        get: |r| &r.reg_password2,
    },
    TypingField {
        screen: Screen::Login,
        name: "host",
        get: |r| &r.host_input,
    },
    TypingField {
        screen: Screen::Login,
        name: "domain",
        get: |r| &r.domain_input,
    },
    TypingField {
        screen: Screen::Characters,
        name: "create_name",
        get: |r| &r.create_name,
    },
    TypingField {
        screen: Screen::Characters,
        name: "delete_name",
        get: |r| &r.delete_name,
    },
    TypingField {
        screen: Screen::Characters,
        name: "host",
        get: |r| &r.host_input,
    },
    TypingField {
        screen: Screen::Characters,
        name: "domain",
        get: |r| &r.domain_input,
    },
    TypingField {
        screen: Screen::Game,
        name: "chat",
        get: |r| &r.chat_input,
    },
    TypingField {
        screen: Screen::Game,
        name: "chat_filter",
        get: |r| &r.chat_filter,
    },
    TypingField {
        screen: Screen::Game,
        name: "guild",
        get: |r| &r.guild_input,
    },
    TypingField {
        screen: Screen::Game,
        name: "host",
        get: |r| &r.host_input,
    },
    TypingField {
        screen: Screen::Game,
        name: "domain",
        get: |r| &r.domain_input,
    },
    // M24: поля фильтров админ-панели — при наборе не должны
    // срабатывать горячие клавиши (C/I/… открывали окна).
    TypingField {
        screen: Screen::Game,
        name: "admin_search",
        get: |r| &r.admin_search,
    },
    TypingField {
        screen: Screen::Game,
        name: "admin_inv_search",
        get: |r| &r.admin_inv_search,
    },
    TypingField {
        screen: Screen::Game,
        name: "admin_player",
        get: |r| &r.admin_player,
    },
];

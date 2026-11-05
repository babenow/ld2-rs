//! Регистрация аккаунта — клиентское ядро (M13.2, D33–D35).
//!
//! Форма и валидация живут здесь (чистые функции, без UI и без знания
//! языка): экран (M13.3) только рендерит и отправляет `cmd/auth.register`.
//! **D34:** клиент проверяет только непустоту полей, совпадение паролей и
//! грубый синтаксис email — точные лимиты приходят кодами сервера
//! (`E_BAD_EMAIL`, `E_WEAK_PASSWORD`…), хардкода лимитов нет (ТЗ §15.4).

use ld2_protocol::ErrorCode;

/// Поле формы регистрации, куда «попадает» ошибка (для inline-подсветки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterField {
    Login,
    Email,
    Password,
    /// Подтверждение пароля (клиентская проверка — сервера это не касается).
    Confirm,
    /// Общая ошибка формы (rate-limit, регистрация выключена, сеть).
    Form,
}

/// Клиентская ошибка формы (только грубые случаи — D34).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterIssue {
    /// Пустое/состоящее из пробелов поле.
    Empty(RegisterField),
    /// Пароль и подтверждение не совпадают.
    PasswordMismatch,
    /// Грубый синтаксис email нарушен (сервер — источник истины, ADR-007).
    BadEmail,
}

impl RegisterIssue {
    /// Поле для inline-подсветки.
    pub fn field(&self) -> RegisterField {
        match self {
            Self::Empty(f) => *f,
            Self::PasswordMismatch => RegisterField::Confirm,
            Self::BadEmail => RegisterField::Email,
        }
    }
}

/// Ошибка формы регистрации для UI: поле (inline-подсветка) + текст
/// (серверный из §9 fallback или локализованный клиентский — D34).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterError {
    pub field: RegisterField,
    pub message: String,
}

/// Маппинг серверного кода ошибки → поле формы (D35: одна таблица в ядре;
/// новые коды без пары → `Form` с текстом сервера из §9 fallback).
pub fn register_field_for(code: &ErrorCode) -> RegisterField {
    match code {
        ErrorCode::LoginTaken => RegisterField::Login,
        ErrorCode::BadEmail | ErrorCode::EmailTaken => RegisterField::Email,
        ErrorCode::WeakPassword => RegisterField::Password,
        _ => RegisterField::Form,
    }
}

/// Значения формы регистрации. Пароль живёт только здесь и в поле ввода —
/// в store/логи/эффекты не попадает (D30-дух).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegisterForm {
    pub login: String,
    pub email: String,
    pub password: String,
    pub confirm: String,
}

impl RegisterForm {
    /// Клиентская валидация (D34): порядок — непустота полей, синтаксис
    /// email, совпадение паролей. `Ok` — можно отправлять серверу.
    pub fn validate(&self) -> Result<(), RegisterIssue> {
        if self.login.trim().is_empty() {
            return Err(RegisterIssue::Empty(RegisterField::Login));
        }
        if self.email.trim().is_empty() {
            return Err(RegisterIssue::Empty(RegisterField::Email));
        }
        if self.password.is_empty() {
            return Err(RegisterIssue::Empty(RegisterField::Password));
        }
        if self.confirm.is_empty() {
            return Err(RegisterIssue::Empty(RegisterField::Confirm));
        }
        if !email_loosely_valid(self.email.trim()) {
            return Err(RegisterIssue::BadEmail);
        }
        if self.password != self.confirm {
            return Err(RegisterIssue::PasswordMismatch);
        }
        Ok(())
    }

    /// Нормализация перед отправкой — повторяет серверную (D33): сервер
    /// нормализует заново, здесь — чтобы не слать явный мусор.
    pub fn normalized_email(&self) -> String {
        self.email.trim().to_lowercase()
    }
}

/// Грубая проверка email (D34): без точности серверной — только очевидные
/// опечатки («нет @», «пробел», «домен без точки»). Финальный вердикт —
/// за сервером (`E_BAD_EMAIL`, ADR-007).
pub fn email_loosely_valid(email: &str) -> bool {
    if email.is_empty() || email.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> RegisterForm {
        RegisterForm {
            login: "player".into(),
            email: "Player@Example.com".into(),
            password: "longenough1".into(),
            confirm: "longenough1".into(),
        }
    }

    #[test]
    fn valid_form_passes_and_normalizes_email() {
        let form = valid();
        assert_eq!(form.validate(), Ok(()));
        assert_eq!(form.normalized_email(), "player@example.com");
    }

    #[test]
    fn empty_fields_detected_in_order() {
        let mut form = valid();
        form.login = "   ".into();
        assert_eq!(
            form.validate(),
            Err(RegisterIssue::Empty(RegisterField::Login))
        );

        let mut form = valid();
        form.email = "".into();
        assert_eq!(
            form.validate(),
            Err(RegisterIssue::Empty(RegisterField::Email))
        );

        let mut form = valid();
        form.password = "".into();
        assert_eq!(
            form.validate(),
            Err(RegisterIssue::Empty(RegisterField::Password))
        );

        let mut form = valid();
        form.confirm = "".into();
        assert_eq!(
            form.validate(),
            Err(RegisterIssue::Empty(RegisterField::Confirm))
        );
    }

    #[test]
    fn password_mismatch_detected_after_email() {
        let mut form = valid();
        form.confirm = "other".into();
        assert_eq!(form.validate(), Err(RegisterIssue::PasswordMismatch));
        assert_eq!(
            RegisterIssue::PasswordMismatch.field(),
            RegisterField::Confirm
        );
    }

    #[test]
    fn loose_email_rejects_obvious_typos() {
        for bad in ["no-at", "a@b", "@b.c", "a b@c.d", "a@.b", ""] {
            assert!(!email_loosely_valid(bad), "отклонён: {bad:?}");
        }
        for ok in ["a@b.co", "x+y@sub.example.org", "пользователь@пример.рф"] {
            assert!(email_loosely_valid(ok), "принят: {ok:?}");
        }
    }

    #[test]
    fn server_error_codes_map_to_fields() {
        assert_eq!(
            register_field_for(&ErrorCode::LoginTaken),
            RegisterField::Login
        );
        assert_eq!(
            register_field_for(&ErrorCode::EmailTaken),
            RegisterField::Email
        );
        assert_eq!(
            register_field_for(&ErrorCode::BadEmail),
            RegisterField::Email
        );
        assert_eq!(
            register_field_for(&ErrorCode::WeakPassword),
            RegisterField::Password
        );
        // Непарные коды — общая ошибка формы (текст сервера как есть).
        for code in [
            ErrorCode::RegisterDisabled,
            ErrorCode::RateLimited,
            ErrorCode::Internal,
        ] {
            assert_eq!(register_field_for(&code), RegisterField::Form, "{code:?}");
        }
    }
}

//! Главное окно приложения (eframe 0.37).
//!
//! Цикл кадра: дренаж ingress-канала транспорта → [`AppStore::apply`] →
//! исполнение [`Effect`] (keyring/токен/конфиг) → отрисовка экрана по
//! `store.screen` → плановый опрос канала (`request_repaint_after`).
//!
//! egui 0.37: корневой `Ui` приходит в `App::ui`; панели — `egui::Panel::top`.

use std::time::Duration;

use eframe::egui;
use tokio::sync::mpsc;

use crate::config::{Config, ConfigStore};
use crate::l10n::{self, Key, Lang};
use crate::secrets;
use crate::store::{AppStore, Effect, Screen};
use crate::transport::{self, IngressEvent, TransportConfig, TransportHandle};
use crate::ui::{self, Forms, UiCtx};

pub struct App {
    lang: Lang,
    config: Config,
    config_store: ConfigStore,
    store: AppStore,
    transport: TransportHandle,
    ingress: mpsc::UnboundedReceiver<IngressEvent>,
    forms: Forms,
}

impl App {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        config: Config,
        config_store: ConfigStore,
    ) -> Self {
        // egui использует встроенный (embedded) шрифт Ubuntu-Light с кириллицей
        // в epaint — не полагаемся на системные шрифты (ТЗ §7.5).
        let mut tcfg = TransportConfig::new(config.host.clone());
        let mut forms = Forms::default();

        // Keyring-resume (ФР-01, А4): входили раньше — экран входа пропускаем,
        // транспорт сам сделает auth.resume → session.start (§4.5).
        if let Some(login) = &config.last_login {
            forms.login.login = login.clone();
            // Чтение keyring с таймаутом: на macOS запрос к keychain может
            // ждать диалога доступа для нового бинаря (инцидент M4/M5) —
            // приложение обязано стартовать, а не виснуть до ответа.
            if let Some(token) =
                read_token_with_timeout(&config.host, login, Duration::from_secs(2))
            {
                tcfg.refresh_token = Some(token);
                tracing::info!("keyring: resume-токен найден ({login})");
            }
        }

        let (tx, rx) = mpsc::unbounded_channel();
        let transport = transport::spawn(tcfg, tx);

        Self {
            lang: Lang::from_code(&config.lang),
            config,
            config_store,
            store: AppStore::new(),
            transport,
            ingress: rx,
            forms,
        }
    }

    /// Опрос ingress-канала транспорта (поток транспорта не имеет доступа к
    /// egui-контексту — поэтому polling с `request_repaint_after`).
    fn drain_ingress(&mut self) {
        for _ in 0..64 {
            match self.ingress.try_recv() {
                Ok(event) => {
                    let effects = self.store.apply(event);
                    self.exec_effects(effects);
                }
                Err(_) => break,
            }
        }
    }

    fn exec_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::PersistSession {
                    login,
                    refresh_token,
                } => {
                    if login.trim().is_empty() {
                        // Не храним пустой логин: иначе keyring-ключ `<host>:` —
                        // мусор, а чтение может упереться в диалог доступа macOS.
                        tracing::warn!("PersistSession с пустым логином — пропускаю");
                    } else {
                        if let Err(e) =
                            secrets::set_token(&self.config.host, &login, &refresh_token)
                        {
                            tracing::warn!("keyring: не удалось сохранить токен: {e}");
                        }
                        self.config.last_login = Some(login);
                        if let Err(e) = self.config_store.save(&self.config) {
                            tracing::warn!("не удалось сохранить конфиг: {e:#}");
                        }
                    }
                }
                Effect::ForgetSession { login } => {
                    match secrets::delete_token(&self.config.host, &login) {
                        Ok(()) | Err(keyring::Error::NoEntry) => {
                            tracing::info!("keyring: токен удалён ({login})");
                        }
                        Err(e) => tracing::warn!("keyring: не удалось удалить токен: {e}"),
                    }
                }
                Effect::TransportToken(token) => {
                    if let Err(e) = self.transport.set_refresh_token(Some(token)) {
                        tracing::warn!("транспорт недоступен для ротации токена: {e}");
                    }
                }
            }
        }
    }

    fn toggle_lang(&mut self) {
        self.lang = self.lang.toggle();
        self.config.lang = self.lang.code().to_string();
        if let Err(e) = self.config_store.save(&self.config) {
            tracing::error!("не удалось сохранить конфиг: {e:#}");
        } else {
            tracing::info!("язык переключён на {}", self.lang.code());
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let status = ui::screens::status_text(self.lang, &self.store.transport);
        egui::Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(l10n::t(self.lang, Key::AppTitle));
                ui.separator();
                ui.weak(l10n::t(self.lang, Key::Subtitle));
                ui.weak(format!("· {status}"));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(l10n::t(self.lang, Key::Exit)).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    let label = format!(
                        "{}: {}",
                        l10n::t(self.lang, Key::Language),
                        self.lang.code().to_uppercase()
                    );
                    if ui.button(label).on_hover_text("RU / EN").clicked() {
                        self.toggle_lang();
                    }
                });
            });
        });
    }
}

/// Чтение токена keyring с таймаутом: зависший диалог доступа macOS не должен
/// блокировать запуск (украденное время — только этот вызов).
fn read_token_with_timeout(
    host: &str,
    login: &str,
    timeout: std::time::Duration,
) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let host = host.to_string();
    let login = login.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(secrets::get_token(&host, &login));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(token)) => Some(token),
        Ok(Err(keyring::Error::NoEntry)) => {
            tracing::debug!("keyring: токена нет — нужен вход");
            None
        }
        Ok(Err(e)) => {
            tracing::warn!("keyring: ошибка чтения: {e}");
            None
        }
        Err(_) => {
            tracing::warn!(
                "keyring: нет ответа за {timeout:?} — стартую без resume \
                 (диалог доступа macOS отложен)"
            );
            None
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_ingress();
        self.header(ui);

        let screen = self.store.screen;
        let mut ctx = UiCtx {
            store: &mut self.store,
            forms: &mut self.forms,
            transport: &self.transport,
            lang: self.lang,
        };
        let effects = match screen {
            Screen::Login => ui::screens::login(&mut ctx, ui),
            Screen::Characters => ui::screens::characters(&mut ctx, ui),
            Screen::Game => ui::world::game(&mut ctx, ui),
        };
        self.exec_effects(effects);

        // Опрашиваем ingress раз в 100 мс: событий может не быть долго.
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Обёртка чтения keyring обязана вернуться за таймаут даже при
    /// зависшем диалоге доступа (инцидент M4/M5).
    #[test]
    fn keyring_timeout_wrapper_returns_none_for_missing_entry() {
        let start = std::time::Instant::now();
        let token = read_token_with_timeout(
            "ws://test-never:1/ws",
            "no-such-login",
            Duration::from_secs(2),
        );
        assert!(token.is_none(), "нет записи → None");
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "обёртка не зависает: {:?}",
            start.elapsed()
        );
    }
}

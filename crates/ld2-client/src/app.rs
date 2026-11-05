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
    /// Применённые тема/масштаб (применяем при изменении, а не каждый кадр).
    applied_theme: String,
    applied_font_scale: f32,
    /// Базовые размеры шрифта — от них считаем масштаб (без накопления).
    base_text_styles: std::collections::BTreeMap<egui::TextStyle, egui::FontId>,
    /// Были ли события в прошлом кадре (idle-backoff ≤ 5 fps, НФР §8).
    had_events_last_frame: bool,
    /// Канал фоновой проверки обновлений (НФР: не блокирует старт).
    update_rx: std::sync::mpsc::Receiver<String>,
    /// Есть ли доступная версия (бейдж в header, §12).
    update_available: Option<String>,
    /// Метка «первый кадр» уже записана (замер cold start, НФР §8).
    first_frame_logged: bool,
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
        // ФР-19: состояние UI (вкладка листа, пати-виджет) из конфига.
        forms.sheet.tab = crate::ui::SheetTab::parse_tab(&config.sheet_tab);
        forms.party_collapsed = config.party_collapsed;
        forms.host_draft = config.host.clone();

        // Keyring-resume (ФР-01, А4): входили раньше — экран входа пропускаем,
        // транспорт сам сделает auth.resume → session.start (§4.5).
        if let Some(login) = &config.last_login {
            forms.login.login = login.clone();
            // Чтение keyring с таймаутом: на macOS запрос к keychain может
            // ждать диалога доступа для нового бинаря (инцидент M4/M5) —
            // приложение обязано стартовать, а не виснуть до ответа.
            // 1с: холодный старт ≤ 2с (НФР) — таймаут защищает от
            // зависания на диалоге keychain, но не должен его съедать.
            if let Some(token) =
                secrets::get_token_with_timeout(&config.host, login, Duration::from_secs(1))
            {
                tcfg.refresh_token = Some(token);
                tracing::info!("keyring: resume-токен найден ({login})");
            }
        }

        let (tx, rx) = mpsc::unbounded_channel();
        let transport = transport::spawn(tcfg, tx);

        let mut store = AppStore::new();
        // НФР §8/ФР-19: лимит истории чата — из конфига.
        store.chat_limit = config.chat_limit;

        // НФР «Обновления»: проверка GitHub Releases в фоне (не блокирует).
        let (update_tx, update_rx) = std::sync::mpsc::channel();
        crate::update_check::spawn_check(update_tx);

        Self {
            applied_theme: config.theme.clone(),
            applied_font_scale: config.font_scale,
            had_events_last_frame: false,
            update_rx,
            update_available: None,
            first_frame_logged: false,
            base_text_styles: _cc.egui_ctx.style_of(egui::Theme::Dark).text_styles.clone(),
            lang: Lang::from_code(&config.lang),
            config,
            config_store,
            store,
            transport,
            ingress: rx,
            forms,
        }
    }

    /// Применение темы/масштаба шрифта из конфига (ФР-19): при изменении,
    /// от базовых стилей старта — масштаб не накапливается.
    fn apply_visual_prefs(&mut self, ctx: &egui::Context) {
        if self.config.theme != self.applied_theme {
            // Жёсткая привязка к выбору пользователя (не к системной теме).
            let pref = if self.config.theme == "light" {
                egui::ThemePreference::Light
            } else {
                egui::ThemePreference::Dark
            };
            ctx.set_theme(pref);
            self.applied_theme = self.config.theme.clone();
            tracing::info!("тема применена: {}", self.applied_theme);
        }
        if (self.config.font_scale - self.applied_font_scale).abs() > f32::EPSILON {
            let scale = self.config.font_scale;
            // egui 0.37: стили per-theme — применяем к обеим темам.
            for theme in [egui::Theme::Dark, egui::Theme::Light] {
                let mut style = (*ctx.style_of(theme)).clone();
                style.text_styles = self
                    .base_text_styles
                    .iter()
                    .map(|(k, v)| {
                        // НФР доступность: min font scale 12px.
                        (
                            k.clone(),
                            egui::FontId::new((v.size * scale).max(12.0), v.family.clone()),
                        )
                    })
                    .collect();
                ctx.set_style_of(theme, style);
            }
            self.applied_font_scale = scale;
            tracing::info!("масштаб шрифта применён: {scale:.2}");
        }
    }

    /// Смена хоста в настройках (ФР-19): переподключение транспорта.
    /// Токен keyring хранится per-host — на новом сервере будет чистый вход.
    fn reconnect_transport(&mut self) {
        let host = self.config.host.clone();
        tracing::info!("смена сервера → переподключение: {host}");
        self.transport.close();
        // Токены keyring привязаны к хосту: на новом сервере — чистый вход
        // (без resume — старый refresh невалиден для нового адреса).
        let tcfg = TransportConfig::new(host);
        let (tx, rx) = mpsc::unbounded_channel();
        self.transport = transport::spawn(tcfg, tx);
        self.ingress = rx;
        // Чистый старт на новом сервере: экран входа, без старого состояния.
        self.store.to_login_screen();
    }

    /// Опрос ingress-канала транспорта (поток транспорта не имеет доступа к
    /// egui-контексту — поэтому polling с `request_repaint_after`).
    fn drain_ingress(&mut self) -> bool {
        let mut had_events = false;
        for _ in 0..64 {
            match self.ingress.try_recv() {
                Ok(event) => {
                    had_events = true;
                    let effects = self.store.apply(event);
                    self.exec_effects(effects);
                }
                Err(_) => break,
            }
        }
        had_events
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
                Effect::Notify(kind) => {
                    // D7: под флагом настроек (desktop.notify).
                    if self.config.notify {
                        let body = l10n::notify_body(self.lang, kind);
                        crate::notify::show(&body);
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
                if let Some(rtt) = self.store.rtt_ms {
                    // НФР §8: RTT-индикатор (обновляется по ack).
                    ui.weak(format!("· {rtt} мс"));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(l10n::t(self.lang, Key::Exit)).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.button(l10n::t(self.lang, Key::SettingsBtn)).clicked() {
                        self.forms.settings_open = !self.forms.settings_open;
                    }
                    // §12: бейдж обновления — открытие страницы релизов.
                    if let Some(version) = self.update_available.clone() {
                        let label = l10n::tf(
                            self.lang,
                            Key::UpdateBadge,
                            &[("{v}", &format!("v{version}"))],
                        );
                        if ui
                            .colored_label(egui::Color32::from_rgb(0x98, 0xC3, 0x79), label)
                            .on_hover_text(crate::update_check::RELEASES_PAGE)
                            .clicked()
                        {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(
                                crate::update_check::RELEASES_PAGE,
                            ));
                        }
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

/// Локализованное тело уведомления (D7: события — в `l10n`).
impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // НФР §8 (cold start): метка первого кадра — точка замера в логе
        // (между «старт» в main и этой строкой).
        if !self.first_frame_logged {
            self.first_frame_logged = true;
            tracing::info!("первый кадр отрисован");
        }
        let had_events = self.drain_ingress();
        self.had_events_last_frame = had_events;
        // Результат фоновой проверки обновлений (§12).
        while let Ok(version) = self.update_rx.try_recv() {
            if crate::update_check::is_newer(&version, env!("CARGO_PKG_VERSION")) {
                tracing::info!("доступна версия v{version}");
                self.update_available = Some(version);
            }
        }
        self.apply_visual_prefs(ui.ctx());
        self.header(ui);

        let screen = self.store.screen;
        let mut ctx = UiCtx {
            store: &mut self.store,
            forms: &mut self.forms,
            transport: &self.transport,
            lang: self.lang,
            cfg: &mut self.config,
            cfg_store: &self.config_store,
        };
        let effects = match screen {
            Screen::Login => ui::screens::login(&mut ctx, ui),
            Screen::Characters => ui::screens::characters(&mut ctx, ui),
            Screen::Game => ui::world::game(&mut ctx, ui),
        };
        self.exec_effects(effects);

        // Окно настроек (ФР-19) — поверх любого экрана.
        if self.forms.settings_open {
            let mut ctx = UiCtx {
                store: &mut self.store,
                forms: &mut self.forms,
                transport: &self.transport,
                lang: self.lang,
                cfg: &mut self.config,
                cfg_store: &self.config_store,
            };
            crate::ui::settings::modal(&mut ctx, ui);
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.forms.settings_open = false;
            }
        }

        // Смена хоста применена — переподключаемся после кадра (ФР-19).
        if std::mem::take(&mut self.forms.host_apply_pending) {
            self.reconnect_transport();
        }

        // НФР §8: idle ≤ 5 fps (пустой дренаж → 200 мс), при событиях — 50 мс.
        let idle = self.store.transport == crate::transport::TransportStatus::Online
            && !self.had_events_last_frame;
        let delay = if idle {
            Duration::from_millis(200)
        } else {
            Duration::from_millis(50)
        };
        ui.ctx().request_repaint_after(delay);
    }
}

//! Корневой view gpui-клиента (M10.3): шапка, экраны входа/персонажей/мира,
//! окно настроек и `tick` — опрос ingress-канала транспорта + исполнение
//! [`Effect`] (аналог `App::ui` egui-клиента, D12: логика в `ld2-core`).

use std::time::Duration;

use gpui::{
    div, prelude::*, px, rgb, Context, Div, Entity, MouseButton, Render, SharedString, Window,
};
use ld2_core::config::{host_allowed, Config, ConfigStore};
use ld2_core::l10n::{self, t, tf, Key, Lang};
use ld2_core::store::{Act, AppStore, Effect, Screen};
use ld2_core::transport::{self, IngressEvent, TransportConfig, TransportHandle, TransportStatus};
use ld2_protocol::cmd::{AuthLogin, CharSelect, LocationMove};

mod game;

use crate::palette::{palette, Palette};
use crate::text_input::TextInput;

/// Release-сборка: внешний `ws://` запрещён (НФР безопасность, guard M10.1).
const RELEASE: bool = !cfg!(debug_assertions);

/// Что делать по клику (замыкания в listener должны быть `Fn` — действуем
/// по копируемому перечислению).
#[derive(Clone)]
enum Click {
    Login,
    Logout,
    Exit,
    ToggleLang,
    ToggleSettings,
    CloseSettings,
    Theme(&'static str),
    ChatLimit(i64),
    ToggleNotify,
    ApplyHost,
    SelectChar(String),
    MoveExit(String),
    OpenReleases,
    // M10.4: игровой экран
    Attack(String),
    Flee,
    Examine(String),
    PartyInvite(String),
    TradeOpen(String),
    RuneCast {
        staff_uid: String,
        socket_index: u32,
    },
    UseItem(String),
    PowerStrike,
    ConfirmDeath,
    LootTake,
    /// Лут трупа игрока: открыть подтверждение (uid, name) — ФР-11.
    LootTakeMarker(String, String),
    LootCancel,
    CloseDetail,
    SwitchChar,
}

/// Инициализация корневого view (всё, что создаётся вне gpui-контекста).
pub struct Shell {
    pub store: AppStore,
    pub config: Config,
    pub config_store: ConfigStore,
    pub lang: Lang,
    pub transport: TransportHandle,
    pub ingress: mpsc::UnboundedReceiver<IngressEvent>,
    pub update_rx: std::sync::mpsc::Receiver<String>,
}

use tokio::sync::mpsc;

pub struct Root {
    store: AppStore,
    config: Config,
    config_store: ConfigStore,
    lang: Lang,
    transport: TransportHandle,
    ingress: mpsc::UnboundedReceiver<IngressEvent>,
    update_rx: std::sync::mpsc::Receiver<String>,
    update_available: Option<String>,
    login_input: Entity<TextInput>,
    password_input: Entity<TextInput>,
    host_input: Entity<TextInput>,
    settings_open: bool,
    first_frame: bool,
    /// Открытое контекстное меню сущности (id строки, ПКМ).
    ctx_menu: Option<String>,
}

impl Root {
    pub fn new(cx: &mut Context<'_, Self>, shell: Shell) -> Self {
        let login_input = cx.new(|cx| {
            let mut input = TextInput::new(cx, t(shell.lang, Key::FieldLogin));
            if let Some(last) = &shell.config.last_login {
                input.content = last.clone().into();
            }
            input
        });
        let password_input = cx.new(|cx| TextInput::new(cx, t(shell.lang, Key::FieldPassword)));
        let host_input = cx.new(|cx| {
            let mut input = TextInput::new(cx, &shell.config.host);
            input.content = shell.config.host.clone().into();
            input
        });
        Self {
            store: shell.store,
            config: shell.config,
            config_store: shell.config_store,
            lang: shell.lang,
            transport: shell.transport,
            ingress: shell.ingress,
            update_rx: shell.update_rx,
            update_available: None,
            login_input,
            password_input,
            host_input,
            settings_open: false,
            first_frame: true,
            ctx_menu: None,
        }
    }

    // ── tick: опрос транспорта + эффекты + поля ввода ─────────────────────

    /// Один шаг фонового цикла: дренаж ingress (≤64), исполнение эффектов,
    /// Enter в полях входа, бейдж обновлений. Возвращает задержку до
    /// следующего шага (НФР §8: события → 50 мс, idle → 200 мс ≤ 5 fps).
    pub fn tick(&mut self, cx: &mut Context<'_, Self>) -> Duration {
        let mut had_events = false;
        for _ in 0..64 {
            match self.ingress.try_recv() {
                Ok(ev) => {
                    had_events = true;
                    let effects = self.store.apply(ev);
                    self.exec_effects(effects);
                }
                Err(_) => break,
            }
        }

        // Enter в полях входа = отправка (флаг ставит TextInput).
        if self.store.screen == Screen::Login {
            let submitted = self.login_input.update(cx, |i, _| i.take_submit())
                || self.password_input.update(cx, |i, _| i.take_submit());
            if submitted {
                self.send_login(cx);
            }
        }

        // Результат фоновой проверки обновлений (§12).
        while let Ok(version) = self.update_rx.try_recv() {
            if ld2_core::update_check::is_newer(&version, env!("CARGO_PKG_VERSION")) {
                tracing::info!("доступна версия v{version}");
                self.update_available = Some(version);
            }
        }

        if had_events {
            // gpui перерисовывается только по notify (аналог request_repaint).
            cx.notify();
        }
        Duration::from_millis(if had_events { 50 } else { 200 })
    }

    fn exec_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::PersistSession {
                    login,
                    refresh_token,
                } => {
                    if login.trim().is_empty() {
                        continue;
                    }
                    if let Err(e) =
                        ld2_core::secrets::set_token(&self.config.host, &login, &refresh_token)
                    {
                        tracing::warn!("keyring: не удалось сохранить токен: {e}");
                    }
                    self.config.last_login = Some(login);
                    if let Err(e) = self.config_store.save(&self.config) {
                        tracing::warn!("не удалось сохранить конфиг: {e:#}");
                    }
                }
                Effect::ForgetSession { login } => {
                    match ld2_core::secrets::delete_token(&self.config.host, &login) {
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
                        ld2_core::notify::show(&l10n::notify_body(self.lang, kind));
                    }
                }
            }
        }
    }

    // ── действия ───────────────────────────────────────────────────────────

    fn send_login(&mut self, cx: &Context<'_, Self>) {
        let login = self.login_input.read(cx).content.to_string();
        let password = self.password_input.read(cx).content.to_string();
        if login.trim().is_empty() || password.is_empty() {
            self.store.login_error = Some(t(self.lang, Key::ErrFillFields).to_string());
            return;
        }
        let cmd = AuthLogin {
            login: login.clone(),
            password,
        };
        match self.transport.send(&cmd) {
            Ok(_) => self.store.on_login_sent(&login),
            Err(e) => self.store.login_error = Some(format!("нет связи: {e}")),
        }
    }

    fn select_char(&mut self, char_id: String) {
        let cmd = CharSelect { char_id };
        match self.transport.send(&cmd) {
            Ok(_) => self.store.on_select_sent(),
            Err(e) => self.store.select_error = Some(format!("нет связи: {e}")),
        }
    }

    fn logout(&mut self) {
        let _ = self.transport.send(&ld2_protocol::cmd::AuthLogout {});
        let effects = self.store.logout();
        self.exec_effects(effects);
    }

    /// Смена хоста в настройках (ФР-19): переподключение транспорта.
    /// Токены keyring привязаны к адресу — на новом сервере чистый вход.
    fn reconnect(&mut self) {
        tracing::info!("смена сервера → переподключение: {}", self.config.host);
        self.transport.close();
        let (tx, rx) = mpsc::unbounded_channel();
        self.transport = transport::spawn(TransportConfig::new(self.config.host.clone()), tx);
        self.ingress = rx;
        self.store.to_login_screen();
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config_store.save(&self.config) {
            tracing::warn!("не удалось сохранить конфиг: {e:#}");
        }
    }

    fn on_click(&mut self, click: Click, cx: &mut Context<'_, Self>) {
        match click {
            Click::Login => self.send_login(cx),
            Click::Logout => self.logout(),
            Click::Exit => cx.quit(),
            Click::ToggleLang => {
                self.lang = self.lang.toggle();
                self.config.lang = self.lang.code().to_string();
                self.save_config();
            }
            Click::ToggleSettings => {
                self.settings_open = !self.settings_open;
                self.host_input
                    .update(cx, |i, _| i.content = self.config.host.clone().into());
            }
            Click::CloseSettings => self.settings_open = false,
            Click::Theme(theme) => {
                self.config.theme = theme.to_string();
                self.save_config();
            }
            Click::ChatLimit(delta) => {
                let limit = (self.config.chat_limit as i64 + delta).clamp(100, 10_000) as usize;
                self.config.chat_limit = limit;
                self.store.chat_limit = limit;
                self.save_config();
            }
            Click::ToggleNotify => {
                self.config.notify = !self.config.notify;
                self.save_config();
            }
            Click::ApplyHost => {
                let host = self.host_input.read(cx).content.to_string();
                let host = host.trim().to_string();
                if !host_allowed(&host, RELEASE) {
                    self.store.notice = Some(t(self.lang, Key::SettingsWsDenied).to_string());
                    return;
                }
                if host != self.config.host {
                    self.config.host = host;
                    self.save_config();
                    self.reconnect();
                }
                self.settings_open = false;
            }
            Click::SelectChar(id) => self.select_char(id),
            Click::MoveExit(exit_id) => {
                let cmd = LocationMove { exit_id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::Move),
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::OpenReleases => cx.open_url(ld2_core::update_check::RELEASES_PAGE),
            // ── M10.4: игровой экран ──
            Click::Attack(id) => {
                let cmd = ld2_protocol::cmd::CombatAttack { target_id: id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::Attack),
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
                self.ctx_menu = None;
            }
            Click::Flee => {
                let cmd = ld2_protocol::cmd::CombatFlee {};
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::Attack),
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
                self.ctx_menu = None;
            }
            Click::Examine(id) => {
                let cmd = ld2_protocol::cmd::EntityInfo { entity_id: id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::Examine),
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
                self.ctx_menu = None;
            }
            Click::PartyInvite(player_id) => {
                let cmd = ld2_protocol::cmd::PartyInvite { player_id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_sheet_sent(Act::Party),
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
                self.ctx_menu = None;
            }
            Click::TradeOpen(player_id) => {
                let cmd = ld2_protocol::cmd::TradeOpen { player_id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_sheet_sent(Act::Trade),
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
                self.ctx_menu = None;
            }
            Click::RuneCast {
                staff_uid,
                socket_index,
            } => {
                let cmd = ld2_protocol::cmd::MagicRuneCast {
                    staff_uid,
                    socket_index,
                    target_id: self.store.target.clone(),
                };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::Cast),
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::UseItem(uid) => {
                let cmd = ld2_protocol::cmd::ItemUse {
                    item_uid: uid,
                    target_id: None,
                };
                match self.transport.send(&cmd) {
                    Ok(_) => {
                        self.store.on_world_sent(Act::Drop);
                        self.store.entity_detail = None;
                        self.store.clear_loot_confirm();
                    }
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::PowerStrike => {
                let cmd = ld2_protocol::cmd::SkillUse {
                    skill_id: "power_strike".into(),
                    target_id: self.store.target.clone(),
                };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::Cast),
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::ConfirmDeath => self.store.confirm_death(),
            Click::LootTakeMarker(uid, name) => self.store.set_loot_confirm(uid, name),
            Click::LootTake => {
                if let Some((uid, _)) = self.store.loot_confirm.clone() {
                    self.on_click(Click::UseItem(uid), cx);
                }
            }
            Click::LootCancel => self.store.clear_loot_confirm(),
            Click::CloseDetail => self.store.entity_detail = None,
            Click::SwitchChar => self.store.back_to_characters(),
        }
    }

    // ── элементы ───────────────────────────────────────────────────────────

    fn header(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let status = status_text(self.lang, &self.store.transport);
        let rtt = self.store.rtt_ms;
        let update = self.update_available.clone();
        let lang = self.lang;
        let settings_open = self.settings_open;
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
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(t(lang, Key::AppTitle)),
                    )
                    .child(div().text_color(rgb(pal.muted)).child(status))
                    .children(
                        rtt.map(|ms| div().text_color(rgb(pal.muted)).child(format!("{ms} мс"))),
                    ),
            )
            .child(
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
                    }))
                    .child(btn(
                        cx,
                        format!("{}: {}", t(lang, Key::Language), lang.code().to_uppercase()),
                        pal.panel,
                        pal.text,
                        Click::ToggleLang,
                    ))
                    .child(btn(
                        cx,
                        t(lang, Key::SettingsBtn).to_string(),
                        if settings_open { pal.accent } else { pal.panel },
                        pal.text,
                        Click::ToggleSettings,
                    ))
                    .child(btn(
                        cx,
                        t(lang, Key::Exit).to_string(),
                        pal.danger,
                        0xffffff,
                        Click::Exit,
                    )),
            )
    }

    fn login_screen(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let error = self.store.login_error.clone();
        div()
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
            .child(field_box(pal, self.login_input.clone()))
            .child(field_box(pal, self.password_input.clone()))
            .child(btn(
                cx,
                t(lang, Key::BtnLogin).to_string(),
                pal.accent,
                pal.accent_text,
                Click::Login,
            ))
            .children(error.map(|e| div().text_color(rgb(pal.danger)).child(e)))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child("demo/demo123 · demo2/demo123"),
            )
    }

    fn characters_screen(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let mut root = div()
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .p(px(12.0))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(18.0))
                    .child(t(lang, Key::CharactersTitle)),
            );
        if let Some(err) = self.store.select_error.clone() {
            root = root.child(div().text_color(rgb(pal.danger)).child(err));
        }
        if let Some(session) = self.store.session.clone() {
            let (used, max) = self.store.slot_usage();
            root = root.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(format!("{} {used}/{max}", t(lang, Key::SlotsUsed))),
            );
            for character in &session.characters {
                let id = character.id.clone();
                let name = character.name.clone();
                let location = character.location.clone();
                let summary = character.summary.clone().unwrap_or_default();
                root = root.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(rgb(pal.panel))
                        .border_1()
                        .border_color(rgb(pal.border))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .child(name.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(rgb(pal.muted))
                                        .child(format!("{location} · {summary}")),
                                ),
                        )
                        .child(btn(
                            cx,
                            t(lang, Key::BtnEnter).to_string(),
                            pal.accent,
                            pal.accent_text,
                            Click::SelectChar(id),
                        )),
                );
            }
        }
        root.child(btn(
            cx,
            t(lang, Key::BtnLogout).to_string(),
            pal.panel,
            pal.danger,
            Click::Logout,
        ))
    }

    /// Окно настроек (ФР-19) — как отдельный экран с кнопкой закрытия.
    fn settings_screen(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let theme = self.config.theme.clone();
        let limit = self.config.chat_limit;
        let notify = self.config.notify;
        let host_ok = host_allowed(&self.config.host, RELEASE);
        let mut root = div()
            .flex()
            .flex_col()
            .gap_3()
            .size_full()
            .p(px(16.0))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(18.0))
                    .child(t(lang, Key::SettingsTitle)),
            );

        // Тема.
        root = root.child(row(
            pal,
            t(lang, Key::SettingsTheme).to_string(),
            vec![
                btn(
                    cx,
                    t(lang, Key::ThemeDark).to_string(),
                    if theme == "dark" {
                        pal.accent
                    } else {
                        pal.panel
                    },
                    pal.text,
                    Click::Theme("dark"),
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

        // Лимит истории чата.
        root = root.child(row(
            pal,
            t(lang, Key::SettingsChatLimit).to_string(),
            vec![
                btn(cx, "−500", pal.panel, pal.text, Click::ChatLimit(-500)),
                div().px_2().child(format!("{limit}")),
                btn(cx, "+500", pal.panel, pal.text, Click::ChatLimit(500)),
            ],
        ));

        // Уведомления (D7).
        root = root.child(row(
            pal,
            t(lang, Key::SettingsNotify).to_string(),
            vec![btn(
                cx,
                if notify {
                    t(lang, Key::On).to_string()
                } else {
                    t(lang, Key::Off).to_string()
                },
                if notify { pal.success } else { pal.panel },
                pal.text,
                Click::ToggleNotify,
            )],
        ));

        // Сервер (хост) с guard.
        root = root.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::SettingsHost)),
                )
                .child(field_box(pal, self.host_input.clone()))
                .children((!host_ok).then(|| {
                    div()
                        .text_color(rgb(pal.danger))
                        .child(t(lang, Key::SettingsWsDenied))
                }))
                .child(btn(
                    cx,
                    t(lang, Key::SettingsApply).to_string(),
                    pal.accent,
                    pal.accent_text,
                    Click::ApplyHost,
                )),
        );

        // Горячие клавиши: только отображение (редактирование/применение
        // навигации — M10.4 вместе с game-навигацией).
        root = root.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(t(lang, Key::SettingsHotkeys)),
                )
                .child(hotkey_row(lang, Key::HkNavUp, &self.config.hotkeys.nav_up))
                .child(hotkey_row(
                    lang,
                    Key::HkNavDown,
                    &self.config.hotkeys.nav_down,
                ))
                .child(hotkey_row(
                    lang,
                    Key::HkNavLeft,
                    &self.config.hotkeys.nav_left,
                ))
                .child(hotkey_row(
                    lang,
                    Key::HkNavRight,
                    &self.config.hotkeys.nav_right,
                ))
                .child(hotkey_row(
                    lang,
                    Key::HkCycleTarget,
                    &self.config.hotkeys.cycle_target,
                )),
        );

        // Путь конфига (ФР-19: импорт/экспорт — правка файла вручную).
        if let Ok(dirs) = ld2_core::config::project_dirs() {
            root = root.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(pal.muted))
                    .child(dirs.config_dir().display().to_string()),
            );
        }

        root.child(btn(
            cx,
            t(lang, Key::BtnClose).to_string(),
            pal.accent,
            pal.accent_text,
            Click::CloseSettings,
        ))
    }
}

/// Поля ввода в рамке (бокс рисует владелец — см. [`crate::text_input`]).
fn field_box(pal: &Palette, input: Entity<TextInput>) -> Div {
    div()
        .w(px(320.0))
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(rgb(pal.border))
        .bg(rgb(pal.panel))
        .child(input)
}

/// Строка настройки: подпись слева, контролы справа.
fn row(pal: &Palette, label: String, controls: Vec<Div>) -> Div {
    let mut r = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(pal.panel))
        .border_1()
        .border_color(rgb(pal.border))
        .child(div().child(label));
    let mut controls = controls.into_iter();
    if let Some(first) = controls.next() {
        r = r.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(first)
                .children(controls),
        );
    }
    r
}

fn hotkey_row(lang: Lang, label: Key, spec: &str) -> Div {
    div()
        .flex()
        .flex_row()
        .justify_between()
        .text_size(px(13.0))
        .child(div().text_color(rgb(0x7f848e)).child(t(lang, label)))
        .child(div().child(spec.to_string()))
}

/// Кнопка: статичные цвета + hover, действие — [`Click`].
fn btn(
    cx: &mut Context<'_, Root>,
    label: impl Into<SharedString>,
    bg: u32,
    fg: u32,
    click: Click,
) -> Div {
    btn_hover(cx, label, bg, bg, fg, click)
}

/// Кнопка с отдельным цветом наведения.
fn btn_hover(
    cx: &mut Context<'_, Root>,
    label: impl Into<SharedString>,
    bg: u32,
    hover_bg: u32,
    fg: u32,
    click: Click,
) -> Div {
    let label: SharedString = label.into();
    div()
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .hover(move |s| s.bg(rgb(hover_bg)))
        .child(label)
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.on_click(click.clone(), cx);
                cx.notify();
            }),
        )
}

/// Статус соединения (паритет `ui::screens::status_text` egui-клиента).
fn status_text(lang: Lang, status: &TransportStatus) -> String {
    let key = match status {
        TransportStatus::Connecting => Key::StatusConnecting,
        TransportStatus::Online => Key::StatusOnline,
        TransportStatus::Reconnecting { .. } => Key::StatusReconnecting,
        TransportStatus::AuthExpired => Key::StatusAuthExpired,
        TransportStatus::Stopped => Key::StatusStopped,
    };
    let base = t(lang, key);
    match status {
        TransportStatus::Reconnecting { attempt, delay } => {
            format!("{base} (попытка {attempt}, {delay:?})")
        }
        _ => base.to_string(),
    }
}

impl Render for Root {
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl gpui::IntoElement {
        let pal = palette(&self.config.theme);
        // Автофокус поля логина на первом кадре (экран входа).
        if self.first_frame {
            self.first_frame = false;
            if self.store.screen == Screen::Login {
                let handle = self.login_input.read(cx).focus_handle.clone();
                window.focus(&handle, cx);
            }
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(pal.bg))
            .text_color(rgb(pal.text))
            .child(self.header(cx, &pal))
            .child(if self.settings_open {
                self.settings_screen(cx, &pal)
            } else {
                match self.store.screen {
                    Screen::Login => self.login_screen(cx, &pal),
                    Screen::Characters => self.characters_screen(cx, &pal),
                    Screen::Game => self.game_screen(cx, &pal),
                }
            })
    }
}

//! Корневой view gpui-клиента (M10.3): шапка, экраны входа/персонажей/мира,
//! окно настроек и `tick` — опрос ingress-канала транспорта + исполнение
//! [`Effect`] (аналог `App::ui` egui-клиента, D12: логика в `ld2-core`).

use std::cell::RefCell;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, rgb, AnyElement, App, Context, Div, Entity, Focusable, MouseButton,
    Render, ScrollHandle, SharedString, Window,
};
use ld2_core::config::{host_allowed, Config, ConfigStore};
use ld2_core::l10n::{self, t, tf, Key, Lang};
use ld2_core::store::{local_epoch, Act, AppStore, Effect, Screen};
use ld2_core::transport::{self, IngressEvent, TransportConfig, TransportHandle, TransportStatus};
use ld2_protocol::cmd::{AuthLogin, CharDelete, LocationMove};

mod chars;
mod chat;
mod game;
mod register;
mod sheet;
mod social;

use crate::palette::{palette, Palette};
use ely::feedback::{Toast, ToastViewport, Toaster};
use ely::forms::{HotkeyInput, Input, InputEvent, PasswordInput, Slider, TextInput};
use ely::menus::{DropdownMenu, Menu, MenuItem};
use ely::theme::{Mode, Theme};

/// Release-сборка: внешний `ws://` запрещён (НФР безопасность, guard M10.1).
const RELEASE: bool = !cfg!(debug_assertions);

/// Что делать по клику (замыкания в listener должны быть `Fn` — действуем
/// по копируемому перечислению).
#[derive(Clone)]
pub(crate) enum Click {
    Login,
    /// M13.3: открыть диалог регистрации (ФР-01).
    OpenRegister,
    Logout,
    Exit,
    ToggleLang,
    ToggleSettings,
    CloseSettings,
    Theme(&'static str),
    ChatLimit(i64),
    ToggleNotify,
    ApplyHost,
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
    login_input: Option<Entity<TextInput>>,
    password_input: Option<Entity<TextInput>>,
    /// Поля диалога регистрации (M13.3, D35): живут только в UI-состоянии,
    /// в store/логи не попадают.
    register_open: bool,
    register_submitted: bool,
    reg_login: Option<Entity<TextInput>>,
    reg_email: Option<Entity<TextInput>>,
    reg_password: Option<Entity<TextInput>>,
    reg_password2: Option<Entity<TextInput>>,
    host_input: Option<Entity<TextInput>>,
    settings_open: bool,
    first_frame: bool,

    /// Поле имени новой гильдии (ФР-16).
    guild_input: Option<Entity<TextInput>>,
    /// Открытая вкладка листа (ФР-19: персистентна в config).
    sheet_tab: sheet::SheetTab,
    /// M10.5: чат — ввод, фильтр, активный канал.
    chat_input: Option<Entity<TextInput>>,
    chat_filter: Option<Entity<TextInput>>,
    chat_channel: String,
    /// M10.5: окно сделки — поле золота.
    /// M11.3: золото сделки (NumberInput Ely; фиксируется операцией).
    trade_gold_value: f64,
    /// M11.3: выбранные значения Select (обучение профессии / предмет сделки).
    learn_pick: Option<String>,
    trade_pick: Option<String>,
    /// M11.7 (D20/D21): поле ввода ника и длительность таймера удаления.
    delete_name: Option<Entity<TextInput>>,
    delete_delay: i64,
    /// M11.4: тосты Ely (notice → всплывающее уведомление).
    toaster: Entity<Toaster>,
    last_notice: Option<String>,
    /// M11.4: stick-to-bottom журнала и чата.
    journal_scroll: ScrollHandle,
    chat_scroll: ScrollHandle,
    last_journal_len: usize,
    last_chat_len: usize,
    /// M10.5: свёрнут ли пати-виджет (ФР-19, из конфига).
    party_collapsed: bool,
    /// M10.6: создание/удаление персонажей (ФР-02).
    create_name: Option<Entity<TextInput>>,
    create_profile: ld2_core::store::Profile,
    create_open: bool,
    create_submitted: bool,
    delete_target: Option<(String, String)>,
}

impl Root {
    pub fn new(cx: &mut Context<'_, Self>, shell: Shell) -> Self {
        let sheet_tab = sheet::SheetTab::parse(&shell.config.sheet_tab);
        let party_collapsed = shell.config.party_collapsed;
        Self {
            store: shell.store,
            config: shell.config,
            config_store: shell.config_store,
            lang: shell.lang,
            transport: shell.transport,
            ingress: shell.ingress,
            update_rx: shell.update_rx,
            update_available: None,
            login_input: None,
            password_input: None,
            register_open: false,
            register_submitted: false,
            reg_login: None,
            reg_email: None,
            reg_password: None,
            reg_password2: None,
            host_input: None,
            settings_open: false,
            first_frame: true,
            guild_input: None,
            sheet_tab,
            chat_input: None,
            chat_filter: None,
            chat_channel: "location".into(),
            trade_gold_value: 0.0,
            learn_pick: None,
            trade_pick: None,
            delete_name: None,
            delete_delay: 10,
            toaster: cx.new(|_| Toaster::default()),
            last_notice: None,
            journal_scroll: ScrollHandle::new(),
            chat_scroll: ScrollHandle::new(),
            last_journal_len: 0,
            last_chat_len: 0,
            party_collapsed,
            create_name: None,
            create_profile: ld2_core::store::Profile::default(),
            create_open: false,
            create_submitted: false,
            delete_target: None,
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

        // Результат фоновой проверки обновлений (§12).
        while let Ok(version) = self.update_rx.try_recv() {
            if ld2_core::update_check::is_newer(&version, env!("CARGO_PKG_VERSION")) {
                tracing::info!("доступна версия v{version}");
                self.update_available = Some(version);
            }
        }

        // M11.7: таймер удаления (D20): тикает только на экране персонажей;
        // вход в игру/логаут (смена экрана) → отмена; истёк → char.delete.
        if self.store.pending_delete.is_some() {
            if self.store.screen != Screen::Characters {
                self.store.cancel_delete();
            } else if let Some(char_id) = self.store.tick_delete(local_epoch()) {
                let cmd = CharDelete {
                    char_id: char_id.clone(),
                };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_delete_sent(&char_id),
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
            }
            // Отсчёт на карточке — перерисовка каждый тик.
            cx.notify();
        }

        // M11.4: новый notice → всплывающий тост Ely (текст в экране
        // остаётся до замены — на время жизни тоста возможен дубль).
        if let Some(notice) = self.store.notice.clone() {
            if self.last_notice.as_deref() != Some(notice.as_str()) {
                self.last_notice = Some(notice.clone());
                let toast = Toast::new(notice);
                self.toaster.update(cx, |t, cx| {
                    t.push(toast, cx);
                });
            }
        } else {
            self.last_notice = None;
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
        let login = self.ti(&self.login_input).read(cx).text().to_string();
        let password = self.ti(&self.password_input).read(cx).text().to_string();
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

    /// Поле, созданное на первом кадре (паникующий доступ: render всегда
    /// идёт после инициализации).
    pub(crate) fn ti<'s>(&self, slot: &'s Option<Entity<TextInput>>) -> &'s Entity<TextInput> {
        slot.as_ref().expect("поле создано на первом кадре")
    }

    /// M11.3: поля Ely создаются на первом кадре — `TextInput::new` требует
    /// Window. Enter в логине/чате = отправка через InputEvent::Submit
    /// (поллинг take_submit упразднён); пароль — masked (отклонение M10 снято).
    fn init_inputs(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
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
            None,
        ));
        self.host_input = Some(mk(window, cx, host.clone(), false, Some(host)));
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

    /// Есть ли фокус в любом текстовом поле (навигация не должна
    /// срабатывать при наборе текста).
    pub(crate) fn typing_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        [
            &self.login_input,
            &self.password_input,
            &self.reg_login,
            &self.reg_email,
            &self.reg_password,
            &self.reg_password2,
            &self.host_input,
            &self.guild_input,
            &self.chat_input,
            &self.chat_filter,
            &self.delete_name,
        ]
        .iter()
        .filter_map(|input| input.as_ref())
        .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Обработка глобального нажатия: навигация/цикл целей по хоткеям
    /// из config (ФР-19); не срабатывает вне игры и при наборе текста.
    pub(crate) fn handle_keystroke(
        &mut self,
        ks: &gpui::Keystroke,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.store.screen != Screen::Game || self.typing_focused(window, cx) {
            return;
        }
        for (spec, dir) in [
            (self.config.hotkeys.nav_up.clone(), "north"),
            (self.config.hotkeys.nav_down.clone(), "south"),
            (self.config.hotkeys.nav_left.clone(), "west"),
            (self.config.hotkeys.nav_right.clone(), "east"),
        ] {
            if keystroke_matches(&spec, ks) {
                self.nav_direction(dir);
                cx.notify();
                return;
            }
        }
        if keystroke_matches(&self.config.hotkeys.cycle_target, ks) {
            self.cycle_target();
            cx.notify();
        }
    }

    /// Навигация по направлению (ФР-03; хоткеи из config — ФР-19).
    pub(crate) fn nav_direction(&mut self, dir: &str) {
        let Some(exit) = self.store.snapshot.as_ref().and_then(|s| {
            s.location
                .exits
                .iter()
                .find(|e| e.dir.as_deref() == Some(dir))
                .map(|e| e.id.clone())
        }) else {
            return;
        };
        let cmd = ld2_protocol::cmd::LocationMove { exit_id: exit };
        match self.transport.send(&cmd) {
            Ok(_) => self.store.on_world_sent(Act::Move),
            Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
        }
    }

    /// Цикл целей по Tab (ФР-05).
    pub(crate) fn cycle_target(&mut self) {
        self.store.cycle_target();
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config_store.save(&self.config) {
            tracing::warn!("не удалось сохранить конфиг: {e:#}");
        }
    }

    pub(crate) fn on_click(&mut self, click: Click, cx: &mut Context<'_, Self>) {
        match click {
            Click::Login => self.send_login(cx),
            Click::OpenRegister => {
                self.register_open = true;
                self.register_submitted = false;
                self.store.register_error = None;
            }
            Click::Logout => self.logout(),
            Click::Exit => cx.quit(),
            Click::ToggleLang => {
                self.lang = self.lang.toggle();
                self.config.lang = self.lang.code().to_string();
                self.save_config();
            }
            Click::ToggleSettings => {
                self.settings_open = !self.settings_open;
                let host = self.config.host.clone();
                if let Some(input) = self.host_input.clone() {
                    input.update(cx, |i, cx| i.set_text(host.clone(), cx));
                }
            }
            Click::CloseSettings => self.settings_open = false,
            Click::Theme(theme) => {
                self.config.theme = theme.to_string();
                // M11.5: тема Ely переключается вместе с config (ФР-19).
                Theme::set_mode_now(
                    if theme == "light" {
                        Mode::Light
                    } else {
                        Mode::Dark
                    },
                    cx,
                );
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
                let host = self.ti(&self.host_input).read(cx).text().to_string();
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
            }
            Click::Flee => {
                let cmd = ld2_protocol::cmd::CombatFlee {};
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::Attack),
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::Examine(id) => {
                let cmd = ld2_protocol::cmd::EntityInfo { entity_id: id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_world_sent(Act::Examine),
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::PartyInvite(player_id) => {
                let cmd = ld2_protocol::cmd::PartyInvite { player_id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_sheet_sent(Act::Party),
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
            }
            Click::TradeOpen(player_id) => {
                let cmd = ld2_protocol::cmd::TradeOpen { player_id };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_sheet_sent(Act::Trade),
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
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
        let update = self.update_available.clone();
        let lang = self.lang;
        let settings_open = self.settings_open;
        // M14/F1: статус сервера и текущая локация — прямо в шапке.
        let status = self.store.transport.clone();
        let rtt = self.store.rtt_ms;
        let in_world = self.store.screen == Screen::Game;
        let before_login = self.store.screen == Screen::Login;
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
                // Левая группа: название игры + (в мире) локация и бейджи.
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
                    .children(location.map(|(name, flags)| {
                        let mut row = div().flex().flex_row().items_center().gap_1().child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(pal.muted))
                                .child(format!("· {name}")),
                        );
                        for (tone, token) in location_badges(&flags) {
                            let color = match tone {
                                BadgeTone::Warn => pal.warn,
                                BadgeTone::Success => pal.success,
                                BadgeTone::Muted => pal.muted,
                            };
                            row = row.child(badge(badge_label(lang, &token), color, pal));
                        }
                        row
                    })),
            )
            .child(
                // Правая группа: обновление, статус сервера, язык,
                // настройки и меню шапки (M14/F1).
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
                    .child(status_badge(pal, lang, &status, rtt))
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
                    .child(header_menu(cx, lang, in_world, before_login)),
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
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child("demo/demo123 · demo2/demo123"),
            )
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

        // M11.5: размер шрифта (НФР: 1.0…1.4, min 12px) — Slider Ely
        // пишет Theme.font_scale (отклонение M10 «font_scale не
        // применяется» снято).
        root = root.child(
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
                .child({
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
                                root.save_config();
                            });
                        })
                }),
        );

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
                .child(
                    div()
                        .w(px(320.0))
                        .child(Input::new(self.ti(&self.host_input))),
                )
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
                .child(hotkey_row(
                    cx,
                    lang,
                    Key::HkNavUp,
                    &self.config.hotkeys.nav_up,
                    0,
                ))
                .child(hotkey_row(
                    cx,
                    lang,
                    Key::HkNavDown,
                    &self.config.hotkeys.nav_down,
                    1,
                ))
                .child(hotkey_row(
                    cx,
                    lang,
                    Key::HkNavLeft,
                    &self.config.hotkeys.nav_left,
                    2,
                ))
                .child(hotkey_row(
                    cx,
                    lang,
                    Key::HkNavRight,
                    &self.config.hotkeys.nav_right,
                    3,
                ))
                .child(hotkey_row(
                    cx,
                    lang,
                    Key::HkCycleTarget,
                    &self.config.hotkeys.cycle_target,
                    4,
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

/// Текущая ОС — для разрешения `secondary-` в Keystroke::parse_with_os.
fn host_os() -> gpui::OperatingSystem {
    if cfg!(target_os = "macos") {
        gpui::OperatingSystem::Mac
    } else if cfg!(windows) {
        gpui::OperatingSystem::Windows
    } else {
        gpui::OperatingSystem::Linux
    }
}

/// Нажатие → спецификация (`alt+up`) для записи в config (ФР-19).
fn spec_from_keystroke(ks: &gpui::Keystroke) -> Option<String> {
    let name = ks.key.to_lowercase();
    if name.is_empty() {
        return None;
    }
    let mut spec = String::new();
    if ks.modifiers.alt {
        spec.push_str("alt+");
    }
    if ks.modifiers.control {
        spec.push_str("ctrl+");
    }
    if ks.modifiers.shift {
        spec.push_str("shift+");
    }
    if ks.modifiers.platform {
        spec.push_str("cmd+");
    }
    spec.push_str(&name);
    Some(spec)
}

/// Совпадает ли нажатие со спецификацией хоткея (`alt+up`, `tab`, …).
/// Спецификации из config.toml парсятся gpui-синтаксисом (`alt-up`).
fn keystroke_matches(spec: &str, ks: &gpui::Keystroke) -> bool {
    let normalized = spec.replace('+', "-");
    // gpui-pre 0.3.10: parse требует &'static str — динамические строки
    // из config.toml парсятся через parse_with_os (документированный путь).
    match gpui::Keystroke::parse_with_os(&normalized, host_os()) {
        Ok(want) => {
            want.modifiers.alt == ks.modifiers.alt
                && want.modifiers.control == ks.modifiers.control
                && want.modifiers.shift == ks.modifiers.shift
                && want.modifiers.platform == ks.modifiers.platform
                && want.modifiers.function == ks.modifiers.function
                && want.key.eq_ignore_ascii_case(&ks.key)
        }
        Err(_) => false,
    }
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

/// Строка хоткея: `HotkeyInput` Ely — запись нажатия и очистка Backspace
/// (ФР-19). Приоритет над навигацией даёт сам виджет (фокус-режим),
/// ручная capture-машинка M10 упразднена; значение хранится в config.toml
/// форматом `alt+up` (round-trip через Keystroke::parse_with_os).
fn hotkey_row(cx: &mut Context<'_, Root>, lang: Lang, label: Key, spec: &str, idx: usize) -> Div {
    const IDS: [&str; 5] = [
        "ld2-hk-nav-up",
        "ld2-hk-nav-down",
        "ld2-hk-nav-left",
        "ld2-hk-nav-right",
        "ld2-hk-cycle",
    ];
    let want = gpui::Keystroke::parse_with_os(&spec.replace('+', "-"), host_os()).ok();
    let this = cx.entity();
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
                .child(t(lang, label)),
        )
        .child(
            HotkeyInput::new(IDS[idx], want).on_change(move |ks, _window, acx| {
                this.update(acx, |root, cx| {
                    let spec = ks
                        .as_ref()
                        .and_then(spec_from_keystroke)
                        .unwrap_or_default();
                    let hk = &mut root.config.hotkeys;
                    match idx {
                        0 => hk.nav_up = spec,
                        1 => hk.nav_down = spec,
                        2 => hk.nav_left = spec,
                        3 => hk.nav_right = spec,
                        _ => hk.cycle_target = spec,
                    }
                    root.save_config();
                    cx.notify();
                });
            }),
        )
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

/// Кнопка с произвольным действием (для листа: замыкание вместо [`Click`]).
pub(crate) fn act_btn(
    cx: &mut Context<'_, Root>,
    label: impl Into<SharedString>,
    bg: u32,
    fg: u32,
    f: impl FnOnce(&mut Root, &mut Context<'_, Root>) + 'static,
) -> Div {
    let cell = RefCell::new(Some(f));
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(bg))
        .text_color(rgb(fg))
        .hover(move |s| s.bg(rgb(bg)))
        .child(label.into())
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                if let Some(f) = cell.borrow_mut().take() {
                    f(this, cx);
                }
                cx.notify();
            }),
        )
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

/// Ключ текста статуса соединения (паритет `ui::screens::status_text`).
fn status_key(status: &TransportStatus) -> Key {
    match status {
        TransportStatus::Connecting => Key::StatusConnecting,
        TransportStatus::Online => Key::StatusOnline,
        TransportStatus::Reconnecting { .. } => Key::StatusReconnecting,
        TransportStatus::AuthExpired => Key::StatusAuthExpired,
        TransportStatus::Stopped => Key::StatusStopped,
    }
}

/// Цвет и ключ статуса сервера для бейджа в шапке (M14/F1): Online →
/// success, Connecting/Reconnecting → warn, AuthExpired/Stopped → danger.
/// Чистая функция (без состояния) — юнит-тест в конце файла.
fn status_color(pal: &Palette, status: &TransportStatus) -> (u32, Key) {
    let color = match status {
        TransportStatus::Online => pal.success,
        TransportStatus::Connecting | TransportStatus::Reconnecting { .. } => pal.warn,
        TransportStatus::AuthExpired | TransportStatus::Stopped => pal.danger,
    };
    (color, status_key(status))
}

/// Текст статуса соединения (с деталью переподключения).
fn status_text(lang: Lang, status: &TransportStatus) -> String {
    let base = t(lang, status_key(status));
    match status {
        TransportStatus::Reconnecting { attempt, delay } => {
            format!("{base} (попытка {attempt}, {delay:?})")
        }
        _ => base.to_string(),
    }
}

/// Тон бейджа (M14/F1): предупреждение/успех/нейтральный.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BadgeTone {
    Warn,
    Success,
    Muted,
}

/// Бейджи у названия локации в шапке: всегда либо `pvp` (warn), либо
/// `safe` (success) — по флагу локации; прочие флаги — muted-бейджи с их
/// сырым текстом. Токены локализует рендер ([`badge_label`]).
/// Чистая функция — юнит-тест в конце файла.
fn location_badges(flags: &[String]) -> Vec<(BadgeTone, String)> {
    let pvp = flags.iter().any(|f| f.eq_ignore_ascii_case("pvp"));
    let mut out = vec![if pvp {
        (BadgeTone::Warn, "pvp".to_string())
    } else {
        (BadgeTone::Success, "safe".to_string())
    }];
    for flag in flags {
        let known = flag.eq_ignore_ascii_case("pvp") || flag.eq_ignore_ascii_case("safe");
        if !known {
            out.push((BadgeTone::Muted, flag.clone()));
        }
    }
    out
}

/// Подпись бейджа: известные токены → ключи l10n, прочее — как есть.
fn badge_label(lang: Lang, token: &str) -> String {
    match token {
        "pvp" => t(lang, Key::BadgePvp).to_string(),
        "safe" => t(lang, Key::BadgeSafe).to_string(),
        other => other.to_string(),
    }
}

/// Плашка в шапке: рамка и текст в тоне, фон — цвет фона окна.
fn badge(label: String, color: u32, pal: &Palette) -> Div {
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(rgb(color))
        .bg(rgb(pal.bg))
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(rgb(color))
        .child(label)
}

/// Бейдж статуса сервера в шапке (M14/F1): цветная точка + текст + RTT
/// (раньше жил в нижнем status_bar, из render убран).
fn status_badge(pal: &Palette, lang: Lang, status: &TransportStatus, rtt: Option<u64>) -> Div {
    let (color, _) = status_color(pal, status);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(rgb(color)))
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.text))
                .child(status_text(lang, status)),
        )
        .children(rtt.map(|ms| {
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(format!("{ms} мс"))
        }))
}

/// Меню шапки (M14/F1, Ely `Menu`): сменить персонажа, выйти из
/// аккаунта, закрыть приложение. Пункты вне контекста отключены:
/// «сменить персонажа» — только в мире, «выйти» — только после входа.
fn header_menu(
    cx: &mut Context<'_, Root>,
    lang: Lang,
    in_world: bool,
    before_login: bool,
) -> DropdownMenu {
    let this = cx.entity();
    let mk = |label: &'static str, click: Click, enabled: bool| {
        let this = this.clone();
        let item = MenuItem::new(label).on_click(move |_window, acx| {
            this.update(acx, |root, cx| root.on_click(click.clone(), cx));
        });
        if enabled {
            item
        } else {
            item.disabled(true)
        }
    };
    let menu = Menu::new()
        .item(mk(t(lang, Key::BtnSwitchChar), Click::SwitchChar, in_world))
        .item(mk(t(lang, Key::BtnLogout), Click::Logout, !before_login))
        .item(mk(t(lang, Key::Exit), Click::Exit, true));
    DropdownMenu::new("ld2-header-menu", t(lang, Key::HeaderMenu), menu)
}

impl Root {
    /// M11.2: on_close диалога — мутация состояния Root и перерисовка
    /// (диалог рендерится из состояния, поэтому «закрыть» = снять флаг).
    pub(crate) fn dismiss(
        &self,
        cx: &mut Context<'_, Self>,
        f: impl Fn(&mut Self) + 'static,
    ) -> impl Fn(&mut Window, &mut App) + 'static {
        let this = cx.entity();
        move |_window, acx| {
            this.update(acx, |root, rcx| {
                f(root);
                rcx.notify();
            });
        }
    }

    /// M11.2: диспетчер всплывающих диалогов (Ely `Dialog` поверх контента,
    /// ADR-004): экран → приоритеты game > social > sheet.
    pub(crate) fn dialog_layer(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if self.settings_open {
            return None;
        }
        // M13.3: диалог регистрации живёт на экране входа (D35) — вне
        // гейта active_dialog (тот смотрит только на экраны мира/персонажей).
        if self.register_open {
            return self.register_dialog(cx, pal);
        }
        // Гейт экрана и приоритет — ядро (E2E-тест M11.6); внутри домена
        // порядок полей зеркалит AppStore::active_dialog.
        let kind = self
            .store
            .active_dialog(self.create_open, self.delete_target.is_some())?;
        match kind {
            ld2_core::store::Dialog::Death
            | ld2_core::store::Dialog::Examine
            | ld2_core::store::Dialog::Loot => self.game_dialog(cx, pal),
            ld2_core::store::Dialog::PartyInvite
            | ld2_core::store::Dialog::GuildInvite
            | ld2_core::store::Dialog::Trade => self.social_dialog(cx, pal),
            ld2_core::store::Dialog::Item
            | ld2_core::store::Dialog::Upgrade
            | ld2_core::store::Dialog::Socket => self.sheet_dialog(cx, pal),
            ld2_core::store::Dialog::Create | ld2_core::store::Dialog::Delete => {
                self.chars_dialog(cx, pal)
            }
        }
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
            self.init_inputs(window, cx);
            if self.store.screen == Screen::Login {
                let handle = self.ti(&self.login_input).read(cx).focus_handle(cx);
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
            .child(
                // Экран — на остаток высоты между шапкой и статус-баром:
                // size_full внутри даёт 100 % этого контейнера, а не окна
                // (иначе низ уходил за границу, баг M11).
                div().flex_grow_1().min_h_0().child(if self.settings_open {
                    self.settings_screen(cx, &pal)
                } else {
                    match self.store.screen {
                        Screen::Login => self.login_screen(cx, &pal),
                        Screen::Characters => self.characters_screen(cx, &pal),
                        Screen::Game => self.game_screen(cx, &pal),
                    }
                }),
            )
            // M14/F1: статус сервера живёт в шапке (status_badge).
            // M11.2: всплывающий диалог поверх контента (скрим + панель).
            .when_some(self.dialog_layer(cx, &pal), |d, dialog| d.child(dialog))
            // M11.4: всплывающие тосты (notice → Ely Toast).
            .child(ToastViewport::new("ld2-toasts", &self.toaster))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M14/F1: цвет и ключ статуса — по роли соединения (3 тона).
    #[test]
    fn status_color_follows_transport_status() {
        let pal = palette("dark");
        let reconnect = TransportStatus::Reconnecting {
            attempt: 2,
            delay: Duration::from_secs(3),
        };
        assert_eq!(
            status_color(&pal, &TransportStatus::Online),
            (pal.success, Key::StatusOnline)
        );
        assert_eq!(
            status_color(&pal, &TransportStatus::Connecting),
            (pal.warn, Key::StatusConnecting)
        );
        assert_eq!(
            status_color(&pal, &reconnect),
            (pal.warn, Key::StatusReconnecting)
        );
        assert_eq!(
            status_color(&pal, &TransportStatus::AuthExpired),
            (pal.danger, Key::StatusAuthExpired)
        );
        assert_eq!(
            status_color(&pal, &TransportStatus::Stopped),
            (pal.danger, Key::StatusStopped)
        );
        // Светлая тема — те же роли цветов.
        let light = palette("light");
        assert_eq!(
            status_color(&light, &TransportStatus::Online).0,
            light.success
        );
        assert_ne!(
            status_color(&light, &TransportStatus::Stopped).0,
            light.success
        );
    }

    /// M14/F1: бейджи локации — PvP по флагу, иначе Safe; прочие — muted.
    #[test]
    fn location_badges_classify_flags() {
        assert_eq!(
            location_badges(&[]),
            vec![(BadgeTone::Success, "safe".to_string())]
        );
        assert_eq!(
            location_badges(&["safe".into()]),
            vec![(BadgeTone::Success, "safe".to_string())]
        );
        assert_eq!(
            location_badges(&["pvp".into()]),
            vec![(BadgeTone::Warn, "pvp".to_string())]
        );
        // Прочие флаги — muted-бейджи после основного, без дублей pvp/safe.
        assert_eq!(
            location_badges(&["pvp".into(), "dark".into()]),
            vec![
                (BadgeTone::Warn, "pvp".to_string()),
                (BadgeTone::Muted, "dark".to_string())
            ]
        );
        assert_eq!(
            location_badges(&["safe".into(), "no_teleport".into()]),
            vec![
                (BadgeTone::Success, "safe".to_string()),
                (BadgeTone::Muted, "no_teleport".to_string())
            ]
        );
    }

    /// M14/F1: подписи бейджей локализуются, неизвестные — как есть.
    #[test]
    fn badge_label_maps_known_tokens() {
        assert_eq!(badge_label(Lang::Ru, "pvp"), "PvP");
        assert_eq!(badge_label(Lang::En, "pvp"), "PvP");
        assert_eq!(badge_label(Lang::Ru, "safe"), "Безопасно");
        assert_eq!(badge_label(Lang::En, "safe"), "Safe");
        assert_eq!(badge_label(Lang::Ru, "dark"), "dark");
    }

    /// Запись хоткея обязана давать спецификацию, парсящуюся обратно
    /// (config.toml ↔ gpui Keystroke, ФР-19).
    #[test]
    fn hotkey_spec_round_trip() {
        for spec in ["alt+up", "tab", "shift+left", "ctrl+f5", "alt+left"] {
            let ks =
                gpui::Keystroke::parse_with_os(&spec.replace('+', "-"), host_os()).expect(spec);
            assert_eq!(
                spec_from_keystroke(&ks).as_deref(),
                Some(spec),
                "round-trip {spec}"
            );
            assert!(keystroke_matches(spec, &ks), "матч {spec}");
        }
        assert!(!keystroke_matches(
            "alt+up",
            &gpui::Keystroke::parse_with_os("up", host_os()).unwrap()
        ));
        assert!(!keystroke_matches(
            "bogus+key",
            &gpui::Keystroke::parse_with_os("up", host_os()).unwrap()
        ));
    }
}

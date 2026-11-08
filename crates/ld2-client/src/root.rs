//! Корневой view gpui-клиента (M10.3): шапка, экраны входа/персонажей/мира,
//! окно настроек и `tick` — опрос ingress-канала транспорта + исполнение
//! [`Effect`] (аналог `App::ui` egui-клиента, D12: логика в `ld2-core`).

use std::cell::RefCell;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, rgb, rgba, AnyElement, App, Context, Div, DragMoveEvent, EmptyView,
    Entity, Focusable, Hsla, KeyDownEvent, MouseButton, Render, ScrollHandle, SharedString, Window,
};
use ld2_core::config::{host_allowed, Config, ConfigStore, Hotkeys};
use ld2_core::l10n::{self, t, tf, Key, Lang};
use ld2_core::store::{local_epoch, local_epoch_ms, Act, AppStore, Effect, Screen};
use ld2_core::transport::{self, IngressEvent, TransportConfig, TransportHandle, TransportStatus};
use ld2_protocol::cmd::{AuthLogin, CharDelete, LocationMove};

mod chars;
mod chat;
mod game;
mod register;
mod sheet;
mod social;

use crate::palette::{palette, Palette};
use ely::buttons::IconButton;
use ely::feedback::{Toast, ToastViewport, Toaster};
use ely::forms::{
    hex, parse_hex, Choice, ColorPicker, HotkeyInput, Input, InputEvent, PasswordInput, Slider,
    Switch, TextInput,
};
use ely::layout::ScrollArea;
use ely::menus::{Menu, MenuItem, OverflowMenu};
use ely::navigation::Tabs;
use ely::primitives::IconName;
use ely::theme::{Mode, Theme};

/// Release-сборка: внешний `ws://` запрещён (НФР безопасность, guard M10.1).
const RELEASE: bool = !cfg!(debug_assertions);

/// M14/F7: какая вкладка активна в блоке журнала (внизу центра).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogTab {
    /// Мир (лог событий) — свободная прокрутка.
    Log,
    /// Чат: каналы + фильтр + ввод (stick-to-bottom остаётся).
    Chat,
}

/// M14/F14: перетаскивание диалога настроек за заголовок (payload drag).
#[derive(Debug)]
struct SettingsDrag;

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

/// Что делать по клику (замыкания в listener должны быть `Fn` — действуем
/// по копируемому перечислению).
#[derive(Clone)]
pub(crate) enum Click {
    Login,
    /// M13.3: открыть диалог регистрации (ФР-01).
    OpenRegister,
    Logout,
    Exit,
    /// M14/F8: выбор языка в настройках («ru»/«en»).
    Lang(&'static str),
    ToggleSettings,
    CloseSettings,
    Theme(&'static str),
    ChatLimit(i64),
    /// M14/F14: флаг «системные уведомления» (слайдер-переключатель).
    SetNotify(bool),
    ApplyHost,
    /// M14/F11: раскрыть ввод нового адреса сервера.
    EditHost,
    /// M14/F13: жирный шрифт строк урона в журнале.
    LogDamageBold(bool),
    /// M14/F14: открыть/закрыть ColorPicker строки (id свотча).
    TogglePicker(&'static str),
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
    /// M11.4: прокрутка журнала (M14/F7 — свободная, см. `journal_*`)
    /// и stick-to-bottom чата.
    journal_scroll: ScrollHandle,
    chat_scroll: ScrollHandle,
    /// M14/F3: обычная прокрутка вкладок листа «Навыки»/«Профессии».
    skills_scroll: ScrollHandle,
    professions_scroll: ScrollHandle,
    /// M14/F7: активная вкладка блока журнала (Лог/Чат), состояние сессии.
    log_tab: LogTab,
    /// M14/F9: вкладка настроек и её прокрутка (состояние сессии).
    settings_tab: SettingsTab,
    settings_scroll: ScrollHandle,
    /// M14/F11: раскрыт ли ввод нового адреса сервера (сессия).
    host_editing: bool,
    /// M14/F14: какой ColorPicker раскрыт под свотчем (id строки).
    color_picker_open: Option<&'static str>,
    /// Смещение диалога настроек от центра (px) — drag.
    settings_delta: (f32, f32),
    /// Смещение указателя внутри заголовка в момент захвата (px).
    settings_drag_offset: Option<(f32, f32)>,
    /// Длина лога на прошлой отрисовке — чтобы приклеивать вниз только
    /// при изменении (M14/F13: лог всегда промотан вниз).
    journal_seen: usize,
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
            skills_scroll: ScrollHandle::new(),
            professions_scroll: ScrollHandle::new(),
            log_tab: LogTab::Log,
            settings_tab: SettingsTab::General,
            settings_scroll: ScrollHandle::new(),
            host_editing: false,
            color_picker_open: None,
            settings_delta: (0.0, 0.0),
            settings_drag_offset: None,
            journal_seen: 0,
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

        // M14/F4: пока есть активный кулдаун (глобальный или любой
        // собственный) — тик 50 мс, чтобы отсчёт на кнопках шёл плавно.
        let now_ms = self.now_ms();
        let cd_active = self.store.global_cooldown_left(now_ms) > 0
            || self
                .store
                .combat_cooldowns
                .values()
                .any(|until_s| until_s.saturating_mul(1000) > now_ms);

        if had_events || cd_active {
            // gpui перерисовывается только по notify (аналог request_repaint).
            cx.notify();
        }
        Duration::from_millis(if had_events || cd_active { 50 } else { 200 })
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
            return;
        }
        // M14/F4: боевые действия панели — те же действия, что кнопки
        // (цифры 1–6; Esc занят диалогами → «Побег» = 4).
        if self.action_hotkey(ks, cx) {
            cx.notify();
        }
    }

    /// M14/F4: боевой хоткей → то же действие, что кнопка панели.
    /// Во время кулдауна действие не отправляется (молча — как
    /// disabled-кнопка с тултипом). Возвращает true, если спецификация
    /// совпала с одним из боевых действий.
    fn action_hotkey(&mut self, ks: &gpui::Keystroke, cx: &mut Context<'_, Self>) -> bool {
        #[derive(Clone, Copy)]
        enum Action {
            Attack,
            PowerStrike,
            Cleave,
            Flee,
            Rune,
            Consumable,
        }
        let hk = self.config.hotkeys.clone();
        let action = if keystroke_matches(&hk.action_attack, ks) {
            Action::Attack
        } else if keystroke_matches(&hk.action_power_strike, ks) {
            Action::PowerStrike
        } else if keystroke_matches(&hk.action_cleave, ks) {
            Action::Cleave
        } else if keystroke_matches(&hk.action_flee, ks) {
            Action::Flee
        } else if keystroke_matches(&hk.action_rune, ks) {
            Action::Rune
        } else if keystroke_matches(&hk.action_consumable, ks) {
            Action::Consumable
        } else {
            return false;
        };

        // Глобальный кулдаун (1 с) блокирует любое боевое действие.
        let now = self.now_ms();
        if self.store.global_cooldown_left(now) > 0 {
            return true;
        }

        match action {
            Action::Attack => {
                if let Some(id) = self.store.target.clone() {
                    self.on_click(Click::Attack(id), cx);
                }
            }
            Action::PowerStrike => {
                // Собственный кулдаун активки + гейт навыка (§4).
                let own = self.own_cd_left_ms("power_strike", now);
                let open = self
                    .store
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| game::power_strike_open(&s.char.skills));
                if AppStore::action_ready(own, 0).0 && open {
                    self.on_click(Click::PowerStrike, cx);
                }
            }
            // Cleave не реализован: кнопка скрыта, хоткей молчит.
            Action::Cleave => {}
            Action::Flee => self.on_click(Click::Flee, cx),
            Action::Rune => {
                if let Some(click) = self.first_rune_click() {
                    self.on_click(click, cx);
                }
            }
            Action::Consumable => {
                if let Some(uid) = self.first_consumable_uid() {
                    self.on_click(Click::UseItem(uid), cx);
                }
            }
        }
        true
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

    /// M14/F14: свотч открывает/закрывает пикер этого цвета (один разом).
    fn toggle_color_picker(&mut self, key: &'static str) {
        self.color_picker_open = match self.color_picker_open {
            Some(open) if open == key => None,
            _ => Some(key),
        };
    }

    /// Цикл целей по Tab (ФР-05).
    pub(crate) fn cycle_target(&mut self) {
        self.store.cycle_target();
    }

    /// Текущее время (epoch, **мс**) с поправкой на часы сервера —
    /// база для отсчёта кулдаунов (M14/F4).
    pub(crate) fn now_ms(&self) -> i64 {
        local_epoch_ms() + self.store.time_offset * 1000
    }

    /// M14/F4: остаток собственного кулдауна действия, мс. Сервер
    /// шлёт `until` в секундах → граница секунды в мс, чтобы отсчёт
    /// на кнопке шёл плавно (тик 50 мс), а не дёргался раз в секунду.
    pub(crate) fn own_cd_left_ms(&self, skill: &str, now_ms: i64) -> i64 {
        let until_s = self.store.combat_cooldowns.get(skill).copied().unwrap_or(0);
        until_s.saturating_mul(1000) - now_ms
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
            Click::Lang(code) => {
                self.lang = Lang::from_code(code);
                self.config.lang = self.lang.code().to_string();
                self.save_config();
            }
            Click::ToggleSettings => {
                self.settings_open = !self.settings_open;
                if !self.settings_open {
                    self.color_picker_open = None;
                }
                let host = self.config.host.clone();
                if let Some(input) = self.host_input.clone() {
                    input.update(cx, |i, cx| i.set_text(host.clone(), cx));
                }
            }
            Click::CloseSettings => self.close_settings(),
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
            Click::SetNotify(notify) => {
                self.config.notify = notify;
                self.save_config();
            }
            Click::EditHost => {
                // Предзаполнение текущим адресом (как при открытии
                // настроек) и раскрытие ввода.
                let host = self.config.host.clone();
                if let Some(input) = self.host_input.clone() {
                    input.update(cx, |i, cx| i.set_text(host, cx));
                }
                self.host_editing = true;
            }
            Click::TogglePicker(key) => {
                self.toggle_color_picker(key);
            }
            Click::LogDamageBold(bold) => {
                self.config.log_damage_bold = bold;
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
                self.host_editing = false;
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
                    Ok(_) => {
                        self.store.on_world_sent(Act::Attack);
                        // M14/F4: любое боевое действие → глобальный кулдаун.
                        self.store.begin_global_cooldown(self.now_ms());
                    }
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::Flee => {
                let cmd = ld2_protocol::cmd::CombatFlee {};
                match self.transport.send(&cmd) {
                    Ok(_) => {
                        self.store.on_world_sent(Act::Attack);
                        self.store.begin_global_cooldown(self.now_ms());
                    }
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
                    Ok(_) => {
                        self.store.on_world_sent(Act::Cast);
                        self.store.begin_global_cooldown(self.now_ms());
                    }
                    Err(e) => self.store.world_error = Some(format!("нет связи: {e}")),
                }
            }
            Click::UseItem(uid) => {
                // M14/F4: расходник боя (свиток/зелье) запускает
                // глобальный кулдаун; лут и прочие предметы — нет.
                let combat_item = self.store.snapshot.as_ref().is_some_and(|s| {
                    s.char.inventory.iter().any(|i| {
                        i.uid == uid && matches!(i.item_type.as_str(), "scroll" | "potion")
                    })
                });
                let cmd = ld2_protocol::cmd::ItemUse {
                    item_uid: uid,
                    target_id: None,
                };
                match self.transport.send(&cmd) {
                    Ok(_) => {
                        self.store.on_world_sent(Act::Drop);
                        if combat_item {
                            self.store.begin_global_cooldown(self.now_ms());
                        }
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
                    Ok(_) => {
                        self.store.on_world_sent(Act::Cast);
                        self.store.begin_global_cooldown(self.now_ms());
                    }
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
                    // M14/F8: выбор языка переехал в настройки.
                    // M14/F10: иконка вместо надписи; подложка accent
                    // помечает открытые настройки (как прежняя кнопка).
                    .child(
                        div()
                            .when(settings_open, |d| d.rounded_md().bg(rgb(pal.accent)))
                            .child(
                                IconButton::new("ld2-header-settings", IconName::Settings)
                                    .tooltip(t(lang, Key::SettingsBtn))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.on_click(Click::ToggleSettings, cx);
                                        cx.notify();
                                    })),
                            ),
                    )
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
    /// M14/F14: тело настроек для диалога `settings_dialog` — без
    /// заголовка и кнопки «Закрыть» (их даёт Dialog: крестик/Esc/скрим).
    /// Фиксированный размер даёт определённую высоту для скролла тела
    /// и влезает в минимум окна 960×640.
    fn settings_content(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let theme = self.config.theme.clone();
        let limit = self.config.chat_limit;
        let notify = self.config.notify;
        let host_ok = host_allowed(&self.config.host, RELEASE);
        // Размер задаёт карточка settings_layer (760×560); здесь —
        // растяжение на остаток, чтобы тело скроллилось внутри.
        let mut root = div().flex().flex_col().gap_3().flex_grow_1().min_h_0();

        // M14/F9: вкладки настроек — «Основные» и «Горячие клавиши»
        // (11 строк ремаппинга растут на своей вкладке и скроллятся).
        let active = match self.settings_tab {
            SettingsTab::General => "general",
            SettingsTab::Fonts => "fonts",
            SettingsTab::Connection => "connection",
            SettingsTab::Hotkeys => "hotkeys",
        };
        // M14/F12: иконки вкладок (метки без изменений).
        let choices = vec![
            Choice::new("general", t(lang, Key::SettingsGeneral)).icon(IconName::SlidersHorizontal),
            Choice::new("fonts", t(lang, Key::SettingsFonts)).icon(IconName::Type),
            Choice::new("connection", t(lang, Key::SettingsConnection)).icon(IconName::Server),
            Choice::new("hotkeys", t(lang, Key::SettingsHotkeys)).icon(IconName::Keyboard),
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
            SettingsTab::General => {
                let mut body = div().flex().flex_col().gap_3();

                // Тема.
                body = body.child(row(
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
            // M14/F14: шрифты — отдельная вкладка (глобальный масштаб,
            // шрифт чата и шрифта лога).
            SettingsTab::Fonts => {
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
            // M14/F11: адрес сервера — read-only; поле ввода
            // раскрывается действием «Подключиться к другому».
            SettingsTab::Connection => {
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
                if self.host_editing {
                    body = body.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .w(px(320.0))
                                    .child(Input::new(self.ti(&self.host_input))),
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
            SettingsTab::Hotkeys => {
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
        };

        // M14/F9: тело активной вкладки — в скролле; заголовок,
        // вкладки и кнопка «Закрыть» остаются вне прокрутки.
        root = root.child(scrollable_tab(
            "ld2-settings-scroll",
            &self.settings_scroll,
            body,
        ));

        root
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
/// M14/F4: строка настройки хоткея — id виджета, ключ l10n и доступ к
/// спецификации в конфиге (геттер/сеттер вместо match по индексу).
struct HotkeyRowSpec {
    id: &'static str,
    label: Key,
    get: fn(&Hotkeys) -> &str,
    set: fn(&mut Hotkeys, String),
}

/// Все ремаппингуемые действия: навигация/цикл (ФР-19) + боевые
/// панели (M14/F4, цифры 1–6 — Esc занят диалогами).
const HOTKEY_ROWS: [HotkeyRowSpec; 11] = [
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

/// M14/F3/F9: содержимое с обычной прокруткой — паттерн `log_block`
/// (ScrollHandle в полях Root, без stick-to-bottom): контейнер занимает
/// остаток высоты, список прокручивается внутри. Используется вкладками
/// листа («Навыки»/«Профессии») и телом настроек.
fn scrollable_tab(id: &'static str, handle: &ScrollHandle, body: Div) -> Div {
    div().flex().flex_col().flex_grow_1().min_h_0().child(
        ScrollArea::new(id)
            .vertical()
            .track_scroll(handle)
            .flex_grow_1()
            .min_h_0()
            .child(body),
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
fn hex_to_u32(text: &str) -> Option<u32> {
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
) -> OverflowMenu {
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
    // M14/F10: иконка-кнопка (⋯-механика Ely) с иконкой-меню вместо
    // надписи; тултип локализуется существующим ключом.
    OverflowMenu::new("ld2-header-menu", menu)
        .icon(IconName::Menu)
        .tooltip(t(lang, Key::HeaderMenu))
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

    /// M14/F14: закрыть настройки (общий путь для Esc/scrim/✕/шапки).
    fn close_settings(&mut self) {
        self.settings_open = false;
        self.color_picker_open = None;
        self.settings_delta = (0.0, 0.0);
        self.settings_drag_offset = None;
    }

    /// M14/F14: настройки — **собственный** диалог поверх экрана:
    /// scrim (клик/Esc/✕ закрывают) и карточка **760×560** с
    /// перетаскиванием за заголовок (`SettingsDrag`).
    ///
    /// Ely `Dialog` не подошёл: его карточка жёстко
    /// `theme.dialog_width()` = 440 px — контент обрезался, и он не
    /// двигается.
    pub(crate) fn settings_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if !self.settings_open {
            return None;
        }
        let lang = self.lang;
        // Центр окна + накопленное смещение перетаскивания.
        let (w, h) = (760.0, 560.0);
        let viewport = window.viewport_size();
        let base_x = ((f32::from(viewport.width) - w) / 2.0).max(0.0);
        let base_y = ((f32::from(viewport.height) - h) / 2.0).max(0.0);
        let left = base_x + self.settings_delta.0;
        let top = base_y + self.settings_delta.1;

        let title = t(lang, Key::SettingsTitle).to_string();
        let content = self.settings_content(cx, pal);
        let titlebar = {
            let drag_this = cx.entity();
            div()
                .id("ld2-settings-titlebar")
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .child(
                    div()
                        .flex_grow_1()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_size(px(16.0))
                        .child(title),
                )
                .child(btn(cx, "✕", pal.panel, pal.text, Click::CloseSettings))
                // Захват: запоминаем смещение указателя внутри заголовка
                // (конструктор on_drag получает offset от gpui).
                .on_drag(SettingsDrag, move |_value, offset, _window, acx| {
                    drag_this.update(acx, |root, cx| {
                        root.settings_drag_offset =
                            Some((f32::from(offset.x), f32::from(offset.y)));
                        cx.notify();
                    });
                    acx.new(|_| EmptyView)
                })
                .on_drag_move(cx.listener(
                    move |root, event: &DragMoveEvent<SettingsDrag>, window, cx| {
                        let Some(offset) = root.settings_drag_offset else {
                            return;
                        };
                        let position = event.event.position;
                        let viewport = window.viewport_size();
                        let base_x = ((f32::from(viewport.width) - w) / 2.0).max(0.0);
                        let base_y = ((f32::from(viewport.height) - h) / 2.0).max(0.0);
                        root.settings_delta.0 = f32::from(position.x) - offset.0 - base_x;
                        root.settings_delta.1 = f32::from(position.y) - offset.1 - base_y;
                        cx.notify();
                    },
                ))
        };
        let card = div()
            .id("ld2-settings-card")
            .absolute()
            .left(px(left))
            .top(px(top))
            .flex()
            .flex_col()
            .gap_3()
            .w(px(w))
            .h(px(h))
            .p(px(16.0))
            .rounded(px(12.0))
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(pal.border))
            .child(titlebar)
            .child(content);

        Some(
            div()
                .id("ld2-settings-layer")
                .size_full()
                .relative()
                .on_key_down(cx.listener(|root, event: &KeyDownEvent, _window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        root.close_settings();
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .id("ld2-settings-scrim")
                        .size_full()
                        .bg(rgba(0x99000000))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|root, _, _, cx| {
                                root.close_settings();
                                cx.notify();
                            }),
                        ),
                )
                .child(card)
                .into_any_element(),
        )
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
                div()
                    .flex_grow_1()
                    .min_h_0()
                    .child(match self.store.screen {
                        Screen::Login => self.login_screen(cx, &pal),
                        Screen::Characters => self.characters_screen(cx, &pal),
                        Screen::Game => self.game_screen(cx, &pal),
                    }),
            )
            // M14/F1: статус сервера живёт в шапке (status_badge).
            // M11.2: всплывающий диалог поверх контента (скрим + панель).
            .when_some(self.dialog_layer(cx, &pal), |d, dialog| d.child(dialog))
            // M14/F14: настройки — диалог поверх экрана (скрим + Esc).
            .when_some(self.settings_layer(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M11.4: всплывающие тосты (notice → Ely Toast).
            .child(ToastViewport::new("ld2-toasts", &self.toaster))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M14/F4: боевые хоткеи — дефолтные цифры 1–6 парсятся и
    /// распознаются так же, как навигационные спецификации.
    #[test]
    fn hotkey_spec_round_trip_covers_action_digits() {
        for spec in ["1", "2", "3", "4", "5", "6"] {
            let ks =
                gpui::Keystroke::parse_with_os(&spec.replace('+', "-"), host_os()).expect(spec);
            assert_eq!(
                spec_from_keystroke(&ks).as_deref(),
                Some(spec),
                "round-trip {spec}"
            );
            assert!(keystroke_matches(spec, &ks), "матч {spec}");
            // Цифра не должна ловиться чужим спецификациям.
            assert!(!keystroke_matches("alt+up", &ks), "не путаем с {spec}");
        }
    }

    /// M14/F4: панель действий настраивается на 11 строк — у каждой
    /// свой id и дефолт из конфига; сеттер пишет только своё поле.
    #[test]
    fn hotkey_rows_cover_nav_and_six_actions() {
        assert_eq!(HOTKEY_ROWS.len(), 11, "навигация/цикл + 6 действий");
        let ids: std::collections::HashSet<_> = HOTKEY_ROWS.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), HOTKEY_ROWS.len(), "id строк уникальны");

        let hk = Hotkeys::default();
        assert_eq!((HOTKEY_ROWS[0].get)(&hk), "alt+up", "навигация");
        assert_eq!((HOTKEY_ROWS[4].get)(&hk), "tab", "цикл целей");
        assert_eq!((HOTKEY_ROWS[5].get)(&hk), "1", "атака");
        assert_eq!((HOTKEY_ROWS[6].get)(&hk), "2", "мощный удар");
        assert_eq!((HOTKEY_ROWS[7].get)(&hk), "3", "раскол (зарезервирован)");
        assert_eq!((HOTKEY_ROWS[8].get)(&hk), "4", "побег (не Esc: диалоги)");
        assert_eq!((HOTKEY_ROWS[9].get)(&hk), "5", "руна");
        assert_eq!((HOTKEY_ROWS[10].get)(&hk), "6", "расходник");

        // Сеттер переписывает только своё поле.
        let mut hk = Hotkeys::default();
        (HOTKEY_ROWS[5].set)(&mut hk, "f5".into());
        assert_eq!(hk.action_attack, "f5");
        assert_eq!(hk.action_flee, "4", "остальные не тронуты");
        assert_eq!((HOTKEY_ROWS[5].get)(&hk), "f5");
    }

    /// M14/F4: гейт «Мощного удара» — владение мечом 5 (skills.md §4).
    #[test]
    fn power_strike_needs_sword_rank_five() {
        use ld2_protocol::types::SkillSnapshot;
        let skill = |id: &str, rank: u32| SkillSnapshot {
            id: id.to_string(),
            name: None,
            rank,
            xp: 0,
            xp_to_next: 120,
        };
        assert!(!game::power_strike_open(&[]), "навыков нет");
        assert!(
            !game::power_strike_open(&[skill("sword", 4)]),
            "ранг 4 — закрыто"
        );
        assert!(
            game::power_strike_open(&[skill("sword", 5)]),
            "ранг 5 — открыто"
        );
        assert!(
            !game::power_strike_open(&[skill("axe", 10)]),
            "чужой навык не открывает"
        );
    }

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

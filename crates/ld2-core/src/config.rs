//! Конфигурация клиента: `<config_dir>/config.toml` (ТЗ ФР-19).
//!
//! Максимум серверных лимитов живёт в `evt/session.start` (protocol.md §5) —
//! здесь только локальные настройки: хост, язык.

use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

/// Dev-адрес мока (PLAN-M0.md Фаза 2). В release-сборке `ws://` запрещён (ТЗ §8).
pub const DEFAULT_HOST: &str = "ws://127.0.0.1:24817/ws";

/// Каталоги данных приложения (macOS: `~/Library/Application Support/ld2/...`).
pub fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("ru", "ld2", "ld2").context("не удалось определить каталоги данных")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Адрес сервера: `wss://` в release, `ws://` — только dev/localhost.
    pub host: String,
    /// Код языка UI: `ru` | `en` (D8: таблицы ru/en с M0).
    pub lang: String,
    /// Последний логин — предзаполнение формы и ключ keyring (ФР-01).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_login: Option<String>,
    /// Тема UI: `dark` | `light` (ФР-19).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Масштаб шрифта 1.0…1.4 — min font scale 12px (НФР доступность).
    #[serde(default = "default_font_scale")]
    pub font_scale: f32,
    /// Размер шрифта **сообщений чата**, px (M14/F8: 10…20, дефолт 12).
    #[serde(default = "default_chat_font_px")]
    pub chat_font_px: f32,
    /// Размер шрифта **строк лога мира**, px (M14/F8: 10…20, дефолт 12).
    #[serde(default = "default_log_font_px")]
    pub log_font_px: f32,
    /// Цвет строк «урон по персонажу» в журнале, hex (M14/F13).
    #[serde(default = "default_log_damage_color")]
    pub log_damage_color: String,
    /// Цвет строк «успешная атака персонажа», hex (M14/F13).
    #[serde(default = "default_log_attack_color")]
    pub log_attack_color: String,
    /// Жирный шрифт строк урона (M14/F13).
    #[serde(default = "default_true")]
    pub log_damage_bold: bool,
    /// OS-уведомления (D7, feature-flag `desktop.notify`).
    #[serde(default = "default_true")]
    pub notify: bool,
    /// Лимит истории чата на канал (НФР §8, настраивается).
    #[serde(default = "default_chat_limit")]
    pub chat_limit: usize,
    /// Состояние UI: открытая вкладка листа (ФР-19: сохраняется).
    #[serde(default = "default_sheet_tab")]
    pub sheet_tab: String,
    /// Состояние UI: свёрнут ли пати-виджет (ФР-19).
    #[serde(default)]
    pub party_collapsed: bool,
    /// Горячие клавиши — ремаппинг (ФР-19): см. [`Hotkeys`].
    #[serde(default)]
    pub hotkeys: Hotkeys,
}

fn default_theme() -> String {
    "dark".into()
}
fn default_chat_font_px() -> f32 {
    12.0
}

fn default_log_font_px() -> f32 {
    12.0
}

fn default_log_damage_color() -> String {
    "#e06c75".into()
}

fn default_log_attack_color() -> String {
    "#61afef".into()
}

/// Hex-цвет: `#rgb` / `#rgba` / `#rrggbb` / `#rrggbbaa`, `#` опционален.
/// Зеркало `ely::forms::parse_hex` — клиент принимает те же формы,
/// какие проходят sanitize (юнит-тест ниже).
pub fn is_hex_color(text: &str) -> bool {
    let digits = text.trim().trim_start_matches('#');
    matches!(digits.len(), 3 | 4 | 6 | 8) && digits.bytes().all(|b| b.is_ascii_hexdigit())
}

fn default_font_scale() -> f32 {
    1.0
}
fn default_true() -> bool {
    true
}
fn default_chat_limit() -> usize {
    5_000
}
fn default_sheet_tab() -> String {
    "skills".into()
}

/// Ремаппинг горячих клавиш (ФР-19): действие → спецификация вида
/// `alt+up` / `tab` (формат — [`crate::ui::hotkey`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hotkeys {
    /// Навигация: север.
    #[serde(default = "hk_alt_up")]
    pub nav_up: String,
    #[serde(default = "hk_alt_down")]
    pub nav_down: String,
    #[serde(default = "hk_alt_left")]
    pub nav_left: String,
    #[serde(default = "hk_alt_right")]
    pub nav_right: String,
    /// Цикл целей (Tab, ФР-05).
    #[serde(default = "hk_tab")]
    pub cycle_target: String,
    // ── M14/F4: боевые действия панели (цифры 1–6; Esc занят
    // диалогами, поэтому «Побег» — не Esc, а 4) ──
    /// Обычная атака (всегда первая кнопка панели).
    #[serde(default = "hk_digit1")]
    pub action_attack: String,
    /// Мощный удар (открывается владением мечом 5).
    #[serde(default = "hk_digit2")]
    pub action_power_strike: String,
    /// Раскол (зарезервирован под cleave — кнопка скрыта).
    #[serde(default = "hk_digit3")]
    pub action_cleave: String,
    /// Побег из боя.
    #[serde(default = "hk_digit4")]
    pub action_flee: String,
    /// Руна посоха.
    #[serde(default = "hk_digit5")]
    pub action_rune: String,
    /// Свиток/зелье (расходник).
    #[serde(default = "hk_digit6")]
    pub action_consumable: String,
    // ── M15/F25: окно персонажа ──
    /// Открыть/закрыть окно персонажа.
    #[serde(default = "hk_char")]
    pub open_character: String,
}

fn hk_alt_up() -> String {
    "alt+up".into()
}
fn hk_alt_down() -> String {
    "alt+down".into()
}
fn hk_alt_left() -> String {
    "alt+left".into()
}
fn hk_alt_right() -> String {
    "alt+right".into()
}
fn hk_tab() -> String {
    "tab".into()
}
fn hk_digit1() -> String {
    "1".into()
}
fn hk_digit2() -> String {
    "2".into()
}
fn hk_digit3() -> String {
    "3".into()
}
fn hk_digit4() -> String {
    "4".into()
}
fn hk_digit5() -> String {
    "5".into()
}
fn hk_digit6() -> String {
    "6".into()
}
fn hk_char() -> String {
    "c".into()
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            nav_up: hk_alt_up(),
            nav_down: hk_alt_down(),
            nav_left: hk_alt_left(),
            nav_right: hk_alt_right(),
            cycle_target: hk_tab(),
            action_attack: hk_digit1(),
            action_power_strike: hk_digit2(),
            action_cleave: hk_digit3(),
            action_flee: hk_digit4(),
            action_rune: hk_digit5(),
            action_consumable: hk_digit6(),
            open_character: hk_char(),
        }
    }
}

/// Все действия ремаппинга: `(название, спецификация)` — для UI настроек.
impl Hotkeys {
    pub fn entries(&mut self) -> Vec<(&'static str, &mut String)> {
        vec![
            ("nav_up", &mut self.nav_up),
            ("nav_down", &mut self.nav_down),
            ("nav_left", &mut self.nav_left),
            ("nav_right", &mut self.nav_right),
            ("cycle_target", &mut self.cycle_target),
            ("action_attack", &mut self.action_attack),
            ("action_power_strike", &mut self.action_power_strike),
            ("action_cleave", &mut self.action_cleave),
            ("action_flee", &mut self.action_flee),
            ("action_rune", &mut self.action_rune),
            ("action_consumable", &mut self.action_consumable),
            ("open_character", &mut self.open_character),
        ]
    }
}

/// Guard безопасности (НФР §8): в release-сборке `ws://` допустим только для
/// localhost (dev-мок); внешние `ws://` — запрещены, нужен `wss://`.
/// `release`-флаг вынесен параметром — тесты проверяют обе ветки.
pub fn host_allowed(host: &str, release: bool) -> bool {
    if !release {
        return true; // dev-сборка: любые адреса (ФР-19)
    }
    if !host.starts_with("ws://") {
        return true; // wss:// и прочее
    }
    host.contains("127.0.0.1") || host.contains("localhost") || host.contains("[::1]")
}

impl Config {
    /// Защита: пустой логин — не логин (иначе keyring ищет мусорную запись,
    /// а macOS может зависнуть на диалоге доступа — M4-инцидент).
    pub fn sanitize(mut self) -> Self {
        if self.last_login.as_deref() == Some("") {
            self.last_login = None;
        }
        // НФР: min font scale 12px ≈ 1.0 от базы; потолок — 1.4 (чтение).
        self.font_scale = self.font_scale.clamp(1.0, 1.4);
        // M14/F8: шрифты чата и лога — 10…20 px (за пределами — дефолт
        // диапазона, как у chat_limit).
        self.chat_font_px = self.chat_font_px.clamp(10.0, 20.0);
        self.log_font_px = self.log_font_px.clamp(10.0, 20.0);
        // M14/F13: битый hex → дефолтный цвет (записан ColorPicker'ом
        // или отредактирован руками в config.toml).
        if !is_hex_color(&self.log_damage_color) {
            self.log_damage_color = default_log_damage_color();
        }
        if !is_hex_color(&self.log_attack_color) {
            self.log_attack_color = default_log_attack_color();
        }
        if !(100..=10_000).contains(&self.chat_limit) {
            self.chat_limit = default_chat_limit();
        }
        if self.theme != "light" {
            self.theme = default_theme();
        }
        self
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            lang: "ru".to_string(),
            last_login: None,
            theme: default_theme(),
            font_scale: default_font_scale(),
            chat_font_px: default_chat_font_px(),
            log_font_px: default_log_font_px(),
            log_damage_color: default_log_damage_color(),
            log_attack_color: default_log_attack_color(),
            log_damage_bold: true,
            notify: default_true(),
            chat_limit: default_chat_limit(),
            sheet_tab: default_sheet_tab(),
            party_collapsed: false,
            hotkeys: Hotkeys::default(),
        }
    }
}

/// Загрузка/сохранение `config.toml`; файл создаётся с дефолтом при первом запуске.
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn open() -> Result<Self> {
        let dirs = project_dirs()?;
        Ok(Self {
            path: dirs.config_dir().join("config.toml"),
        })
    }

    pub fn load(&self) -> Result<Config> {
        if !self.path.exists() {
            let cfg = Config::default();
            self.save(&cfg)?;
            tracing::info!("создан конфиг по умолчанию: {}", self.path.display());
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(&self.path)
            .with_context(|| format!("чтение конфига {}", self.path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("разбор конфига {}", self.path.display()))?;
        Ok(cfg.sanitize())
    }

    pub fn save(&self, cfg: &Config) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(cfg).context("сериализация конфига")?;
        std::fs::write(&self.path, text)
            .with_context(|| format!("запись конфига {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M4-инцидент: пустой last_login вёл к мусорному ключу keyring и
    /// зависанию на диалоге доступа macOS — санитайзим при чтении.
    #[test]
    fn empty_last_login_is_sanitized_to_none() {
        let parsed: Config =
            toml::from_str("host = \"ws://127.0.0.1:24817/ws\"\nlang = \"ru\"\nlast_login = \"\"")
                .expect("разбор");
        assert_eq!(parsed.sanitize().last_login, None, "пустой логин → None");
    }

    /// M9: расширенный конфиг переживает round-trip со старыми файлами
    /// (новые поля — serde defaults).
    #[test]
    fn extended_config_defaults_from_old_file() {
        let old: Config =
            toml::from_str("host = \"ws://127.0.0.1:24817/ws\"\nlang = \"ru\"").expect("разбор");
        let cfg = old.sanitize();
        assert_eq!(cfg.theme, "dark");
        assert_eq!(cfg.font_scale, 1.0);
        assert!(cfg.notify, "D7: уведомления по умолчанию включены");
        assert_eq!(cfg.chat_limit, 5_000);
        assert_eq!(cfg.sheet_tab, "skills");
        assert!(!cfg.party_collapsed);
        assert_eq!(cfg.hotkeys.cycle_target, "tab");
        // M14/F4: боевые хоткеи в старом конфиге берут дефолты 1–6.
        assert_eq!(cfg.hotkeys.action_attack, "1");
        assert_eq!(cfg.hotkeys.action_power_strike, "2");
        assert_eq!(cfg.hotkeys.action_cleave, "3");
        assert_eq!(cfg.hotkeys.action_flee, "4");
        assert_eq!(cfg.hotkeys.action_rune, "5");
        assert_eq!(cfg.hotkeys.action_consumable, "6");
        // M15/F25: окно персонажа — дефолт в старом конфиге.
        assert_eq!(cfg.hotkeys.open_character, "c");
        // M14/F8: шрифты чата/лога в старом конфиге — дефолт 12 px.
        assert_eq!(cfg.chat_font_px, 12.0);
        assert_eq!(cfg.log_font_px, 12.0);
        // M14/F13: цвета/жирность строк боя в старом конфиге — дефолты.
        assert_eq!(cfg.log_damage_color, "#e06c75");
        assert_eq!(cfg.log_attack_color, "#61afef");
        assert!(cfg.log_damage_bold);
    }

    /// M14/F13: hex-цвета принимаются в 4 формах, битые — нет.
    #[test]
    fn hex_color_forms() {
        assert!(is_hex_color("#e06c75"));
        assert!(is_hex_color("e06c75"), "без решётки");
        assert!(is_hex_color("#f0c"), "короткая форма");
        assert!(is_hex_color("#f0ca"));
        assert!(is_hex_color("  #aabbcc  "), "пробелы по краям");
        assert!(!is_hex_color("#e06c7"), "5 цифр");
        assert!(!is_hex_color("#xyzxyz"), "не hex");
        assert!(!is_hex_color(""), "пусто");
        assert!(!is_hex_color("#e06c75zz"));
    }

    /// M14/F13: sanitize откатывает битые цвета на дефолт, валидные —
    /// не трогает (запись ColorPicker'ом проходит без изменений).
    #[test]
    fn sanitize_repairs_log_colors() {
        let bad = Config {
            log_damage_color: "pink".into(),
            log_attack_color: "12".into(),
            ..Config::default()
        }
        .sanitize();
        assert_eq!(bad.log_damage_color, "#e06c75");
        assert_eq!(bad.log_attack_color, "#61afef");

        let ok = Config {
            log_damage_color: "#00ff00".into(),
            log_attack_color: "abc".into(),
            ..Config::default()
        }
        .sanitize();
        assert_eq!(ok.log_damage_color, "#00ff00");
        assert_eq!(ok.log_attack_color, "abc");
    }

    /// M14/F8: sanitize клампит шрифты чата и лога в 10…20 px.
    #[test]
    fn sanitize_clamps_chat_and_log_font_px() {
        let def = Config::default();
        assert_eq!(def.chat_font_px, 12.0, "дефолт 12");
        assert_eq!(def.log_font_px, 12.0, "дефолт 12");

        let out_of_range = Config {
            chat_font_px: 1.0,
            log_font_px: 99.0,
            ..Config::default()
        }
        .sanitize();
        assert_eq!(out_of_range.chat_font_px, 10.0, "пол 10 px");
        assert_eq!(out_of_range.log_font_px, 20.0, "потолок 20 px");

        let in_range = Config {
            chat_font_px: 15.0,
            log_font_px: 17.0,
            ..Config::default()
        }
        .sanitize();
        assert_eq!(in_range.chat_font_px, 15.0, "в диапазоне не трогаем");
        assert_eq!(in_range.log_font_px, 17.0, "в диапазоне не трогаем");
    }

    /// M9: sanitize чинит выходящие за пределы значения.
    #[test]
    fn sanitize_clamps_ranges() {
        let cfg: Config = toml::from_str::<Config>(
            "host = \"wss://x\"\nlang = \"ru\"\nfont_scale = 9.0\nchat_limit = 1\ntheme = \"rainbow\"",
        )
        .expect("разбор")
        .sanitize();
        assert_eq!(cfg.font_scale, 1.4, "потолок масштаба");
        assert_eq!(cfg.chat_limit, 5_000, "лимит вне диапазона → дефолт");
        assert_eq!(cfg.theme, "dark", "неизвестная тема → dark");
    }

    /// НФР безопасность: в release внешний ws:// запрещён, localhost и wss — ок.
    #[test]
    fn host_guard_blocks_external_ws_in_release() {
        assert!(host_allowed("wss://game.example/ws", true));
        assert!(
            host_allowed("ws://127.0.0.1:24817/ws", true),
            "localhost — dev-мок"
        );
        assert!(host_allowed("ws://localhost:24817/ws", true));
        assert!(
            !host_allowed("ws://evil.example/ws", true),
            "внешний ws:// запрещён"
        );
        // Dev-сборка — без ограничений (ФР-19).
        assert!(host_allowed("ws://evil.example/ws", false));
    }

    #[test]
    fn default_config_round_trip() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).expect("сериализация");
        let parsed: Config = toml::from_str(&text).expect("разбор");
        assert_eq!(cfg, parsed);
        assert_eq!(parsed.host, DEFAULT_HOST);
        assert_eq!(parsed.lang, "ru");
    }
}

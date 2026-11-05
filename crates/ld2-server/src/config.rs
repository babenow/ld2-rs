//! Конфигурация сервера: единый файл `server.toml` (D25, PLAN-M12).
//!
//! Все настройки — только здесь (env допускается исключительно для пути к
//! файлу: `--config` / `LD2_SERVER_CONFIG`). Секции `[server]`/`[gameplay]` —
//! единственный источник `evt/session.start` (protocol.md §5): лимиты, фичи,
//! преступность, чат, инвентарь, улучшение, мана.
//!
//! Валидация — fail-fast при старте: [`Config::problems`] собирает **все**
//! проблемы сразу, каждая называет ключ (`server.bind: …`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Полный конфиг `server.toml`. Неизвестные ключи — ошибка (опечатки не
/// должны молча игнорироваться).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerSection,
    pub database: DatabaseSection,
    pub auth: AuthSection,
    pub gameplay: GameplaySection,
    pub world: WorldSection,
    /// Модель опыта навыков (skills.md §2/§8).
    pub skills: SkillsSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    /// `host:port` WS-слушателя (в M12.1 — HTTP `/healthz`).
    pub bind: String,
    /// Имя сервера → `evt/session.start` §5 `server.name`.
    pub name: String,
    /// Регион → §5 `server.region`.
    pub region: String,
    /// Опционально (§5 `server.min_client`): если задан и версия клиента
    /// меньше — вход блокируется. В TOML отсутствует ключ, не `null`.
    #[serde(default)]
    pub min_client: Option<String>,
    /// Уровень логирования: EnvFilter-директива (`"info"`, `"info,sqlx=warn"`).
    pub log_level: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseSection {
    /// `postgres://user:pass@host:5432/db` — секрет, chmod 600, не коммитить.
    pub url: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_max_connections() -> u32 {
    10
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSection {
    pub access_ttl_s: u64,
    pub refresh_ttl_s: u64,
    /// Открытая регистрация `cmd/auth.register` (D28).
    pub register_enabled: bool,
    pub min_login_len: usize,
    pub min_password_len: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameplaySection {
    /// Лимит слотов персонажей → §5 `char_slot_limit` (DoD M2: конфиг, не код).
    pub char_slot_limit: u32,
    pub profession_limit: u32,
    /// Бой (M12.5): тик-луп атак и правила побега.
    pub combat: CombatSection,
    /// Capability-флаги §5: ключи с точками — **в кавычках**:
    /// `"loot.roll" = false`. Неизвестные ключи разрешены (фичи открыты).
    pub features: HashMap<String, FeatureValue>,
    /// Модель преступности §10 — параметры только отсюда.
    pub crime: CrimeSection,
    pub chat: ChatSection,
    pub inventory: InventorySection,
    pub upgrade: UpgradeSection,
    pub magic: MagicSection,
}

/// Значение capability-флага §5: булево или строка (`upgrade.rules`).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum FeatureValue {
    Bool(bool),
    Str(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrimeSection {
    /// Потолок суммарного срока статуса, с.
    pub t_max_s: u64,
    /// Коэффициент «частичного сложения» §10, `(0; 1]`.
    pub blend_k: f64,
    /// Базовые сроки по видам: `murderer_s`, `marauder_s`, …
    pub base: HashMap<String, u64>,
    pub kinds: Vec<CrimeKind>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrimeKind {
    pub kind: String,
    pub title: String,
    pub severity: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatSection {
    /// Размер кольцевой истории в БД (0 — не хранить).
    pub server_history: u32,
    pub max_len: u32,
    pub cooldown_s: f64,
    pub channels: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventorySection {
    pub slots: u32,
    pub equip_slots: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeSection {
    pub max_level: u32,
    pub preview: bool,
    pub shows_chance: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MagicSection {
    /// Режим перезарядки руны: `item` и др. (ADR-002).
    pub rune_recharge: String,
    pub scroll_requires_target: bool,
    pub mana: ManaSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManaSection {
    /// Реген вне боя, с на 1%.
    pub regen_out_s: u64,
    /// Реген в бою, %/с.
    pub regen_in_pct_s: f64,
    /// Стоимость каста руны в мане.
    pub rune_cost: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldSection {
    /// Каталог статического контента (JSON) → сид в PostgreSQL (D26).
    pub content_dir: PathBuf,
    /// Частота тик-лупа мира, Гц (D31), `1..=100`.
    pub tick_hz: u32,
    /// Периодический флаш онлайн-состояния, с (D27).
    pub autosave_interval_s: u64,
}

/// Константы модели опыта навыков (skills.md §8 — «конфиг сервера», D25).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsSection {
    /// `xp_to_next(rank) = round(base_xp × growth^rank)`.
    pub base_xp: i64,
    pub growth: f64,
    /// Потолок ранга (skills.md §2.2) → `E_RANK_CAP`.
    pub rank_max: u32,
    /// Гейты по атрибутам: ранг → требуемое значение (→ `E_REQUIREMENT_ATTR`).
    pub gates: Vec<SkillGate>,
    /// Опыт за убийство (skills.md §2.3): `(base + difficulty × per)`.
    pub kill_xp_base: i64,
    pub kill_xp_per_difficulty: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillGate {
    pub rank: u32,
    pub attr: i64,
}

/// Параметры боя (M12.5, D25 — всё в конфиге).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CombatSection {
    /// Период атак в бою, с (тик-луп `combat.attack`).
    pub attack_interval_s: f64,
    /// Базовый урон персонажа (v1 без формул оружия — см. отклонение).
    pub base_damage: i64,
    /// Шанс успешного побега (`cmd/combat.flee`), `0..=1`.
    pub flee_chance: f64,
}

/// Ошибка загрузки/валидации конфига. Всегда указывает путь к файлу, а
/// [`ConfigError::Invalid`] — имена ключей.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("не удалось прочитать конфиг {}: {err}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        err: std::io::Error,
    },
    #[error("невалидный TOML в {}: {toml_err}", .path.display())]
    Parse {
        path: PathBuf,
        /// В боксе: вариант ≥128 байт на Windows-ABI → clippy
        /// `result_large_err` (CI, -D warnings).
        #[source]
        toml_err: Box<toml::de::Error>,
    },
    #[error("конфиг {} не прошёл валидацию:\n  - {}", .path.display(), .problems.join("\n  - "))]
    Invalid {
        path: PathBuf,
        problems: Vec<String>,
    },
}

impl Config {
    /// Чтение файла → десериализация → валидация (fail-fast).
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|err| ConfigError::Io {
            path: path.to_path_buf(),
            err,
        })?;
        let cfg: Config = toml::from_str(&text).map_err(|toml_err| ConfigError::Parse {
            path: path.to_path_buf(),
            toml_err: Box::new(toml_err),
        })?;
        let problems = cfg.problems();
        if !problems.is_empty() {
            return Err(ConfigError::Invalid {
                path: path.to_path_buf(),
                problems,
            });
        }
        Ok(cfg)
    }

    /// Все проблемы конфига сразу (пустой вектор — конфиг валиден).
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();

        // [server]
        if let Err(msg) = check_host_port(&self.server.bind) {
            out.push(format!("server.bind: {msg}"));
        }
        if self.server.name.trim().is_empty() {
            out.push("server.name: не должно быть пустым".into());
        }
        if self.server.region.trim().is_empty() {
            out.push("server.region: не должно быть пустым".into());
        }
        if tracing_subscriber::EnvFilter::try_new(&self.server.log_level).is_err() {
            out.push(format!(
                "server.log_level: неверная директива {:?} (примеры: \"info\", \"info,sqlx=warn\")",
                self.server.log_level
            ));
        }

        // [database] — значение не логируется и не цитируется в ошибках (D30)
        if !self.database.url.starts_with("postgres://")
            && !self.database.url.starts_with("postgresql://")
        {
            out.push("database.url: ожидается URL, начинающийся с postgres://".into());
        }
        if self.database.max_connections == 0 {
            out.push("database.max_connections: должен быть ≥ 1".into());
        }

        // [auth]
        if self.auth.access_ttl_s < 60 {
            out.push("auth.access_ttl_s: должен быть ≥ 60".into());
        }
        if self.auth.refresh_ttl_s <= self.auth.access_ttl_s {
            out.push("auth.refresh_ttl_s: должен быть больше auth.access_ttl_s".into());
        }
        if self.auth.min_login_len < 3 {
            out.push("auth.min_login_len: должен быть ≥ 3".into());
        }
        if self.auth.min_password_len < 6 {
            out.push("auth.min_password_len: должен быть ≥ 6".into());
        }

        // [gameplay]
        if self.gameplay.char_slot_limit == 0 {
            out.push("gameplay.char_slot_limit: должен быть ≥ 1".into());
        }
        check_features(&self.gameplay.features, &mut out);
        check_crime(&self.gameplay.crime, &mut out);
        check_chat(&self.gameplay.chat, &mut out);
        check_inventory(&self.gameplay.inventory, &mut out);

        if self.gameplay.upgrade.max_level == 0 {
            out.push("gameplay.upgrade.max_level: должен быть ≥ 1".into());
        }

        if self.gameplay.magic.rune_recharge.trim().is_empty() {
            out.push("gameplay.magic.rune_recharge: не должно быть пустым".into());
        }
        if self.gameplay.magic.mana.rune_cost == 0 {
            out.push("gameplay.magic.mana.rune_cost: должен быть ≥ 1".into());
        }
        if self.gameplay.magic.mana.regen_in_pct_s < 0.0 {
            out.push(format!(
                "gameplay.magic.mana.regen_in_pct_s: не может быть отрицательным (получено: {})",
                self.gameplay.magic.mana.regen_in_pct_s
            ));
        }

        // [world]
        if self.world.content_dir.as_os_str().is_empty() {
            out.push("world.content_dir: не должно быть пустым".into());
        }
        if !(1..=100).contains(&self.world.tick_hz) {
            out.push(format!(
                "world.tick_hz: должен быть в 1..=100 (получено: {})",
                self.world.tick_hz
            ));
        }
        if self.world.autosave_interval_s == 0 {
            out.push("world.autosave_interval_s: должен быть ≥ 1".into());
        }

        // [skills] (skills.md §8)
        if self.skills.base_xp < 1 {
            out.push("skills.base_xp: должен быть ≥ 1".into());
        }
        if self.skills.growth <= 1.0 {
            out.push(format!(
                "skills.growth: должен быть > 1.0 (получено: {})",
                self.skills.growth
            ));
        }
        if self.skills.rank_max == 0 {
            out.push("skills.rank_max: должен быть ≥ 1".into());
        }
        let mut gate_ranks = HashSet::new();
        for (i, gate) in self.skills.gates.iter().enumerate() {
            if gate.rank == 0 || gate.rank > self.skills.rank_max {
                out.push(format!(
                    "skills.gates[{i}].rank: должен быть в 1..={} (получено: {})",
                    self.skills.rank_max, gate.rank
                ));
            }
            if gate.attr < 1 {
                out.push(format!("skills.gates[{i}].attr: должен быть ≥ 1"));
            }
            if !gate_ranks.insert(gate.rank) {
                out.push(format!("skills.gates: дубликат ранга {}", gate.rank));
            }
        }
        if self.skills.kill_xp_base < 0 || self.skills.kill_xp_per_difficulty < 0 {
            out.push("skills.kill_xp_*: не может быть отрицательным".into());
        }

        // [gameplay.combat] (M12.5)
        if !(self.gameplay.combat.attack_interval_s >= 0.1
            && self.gameplay.combat.attack_interval_s <= 60.0)
        {
            out.push(format!(
                "gameplay.combat.attack_interval_s: должен быть в 0.1..=60 (получено: {})",
                self.gameplay.combat.attack_interval_s
            ));
        }
        if self.gameplay.combat.base_damage < 1 {
            out.push("gameplay.combat.base_damage: должен быть ≥ 1".into());
        }
        if !(0.0..=1.0).contains(&self.gameplay.combat.flee_chance) {
            out.push(format!(
                "gameplay.combat.flee_chance: должен быть в 0..=1 (получено: {})",
                self.gameplay.combat.flee_chance
            ));
        }

        out
    }
}

/// Известные capability-флаги §5: ключ обязан иметь ожидаемый тип.
/// Неизвестные ключи пропускаются — фичи открыты для расширения сервером.
fn check_features(features: &HashMap<String, FeatureValue>, out: &mut Vec<String>) {
    const BOOL_KEYS: &[&str] = &[
        "loot.roll",
        "respawn.inn",
        "guild.bank",
        "trade.value_hint",
        "chat.slash",
    ];
    const STR_KEYS: &[&str] = &["upgrade.rules"];

    for (key, value) in features {
        if BOOL_KEYS.contains(&key.as_str()) && !matches!(value, FeatureValue::Bool(_)) {
            out.push(format!(
                "gameplay.features.{key}: ожидалось булево значение"
            ));
        }
        if STR_KEYS.contains(&key.as_str()) && !matches!(value, FeatureValue::Str(_)) {
            out.push(format!("gameplay.features.{key}: ожидалась строка"));
        }
    }
}

fn check_crime(crime: &CrimeSection, out: &mut Vec<String>) {
    if crime.t_max_s == 0 {
        out.push("gameplay.crime.t_max_s: должен быть > 0".into());
    }
    if !(crime.blend_k > 0.0 && crime.blend_k <= 1.0) {
        out.push(format!(
            "gameplay.crime.blend_k: должен быть в (0; 1] (получено: {})",
            crime.blend_k
        ));
    }
    if crime.base.is_empty() {
        out.push("gameplay.crime.base: не должно быть пустым".into());
    }
    for (kind, secs) in &crime.base {
        if kind.trim().is_empty() {
            out.push("gameplay.crime.base: пустой ключ".into());
        }
        if *secs == 0 {
            out.push(format!("gameplay.crime.base.{kind}: должен быть > 0"));
        }
    }
    if crime.kinds.is_empty() {
        out.push("gameplay.crime.kinds: не должно быть пустым".into());
    }
    let mut seen = HashSet::new();
    for (i, k) in crime.kinds.iter().enumerate() {
        if k.kind.trim().is_empty() {
            out.push(format!(
                "gameplay.crime.kinds[{i}].kind: не должно быть пустым"
            ));
        }
        if k.title.trim().is_empty() {
            out.push(format!(
                "gameplay.crime.kinds[{i}].title: не должно быть пустым"
            ));
        }
        if k.severity.trim().is_empty() {
            out.push(format!(
                "gameplay.crime.kinds[{i}].severity: не должно быть пустым"
            ));
        }
        if !k.kind.trim().is_empty() && !seen.insert(k.kind.as_str()) {
            out.push(format!("gameplay.crime.kinds: дубликат kind {:?}", k.kind));
        }
    }
}

fn check_chat(chat: &ChatSection, out: &mut Vec<String>) {
    if chat.max_len == 0 {
        out.push("gameplay.chat.max_len: должен быть ≥ 1".into());
    }
    if chat.cooldown_s < 0.0 {
        out.push(format!(
            "gameplay.chat.cooldown_s: не может быть отрицательным (получено: {})",
            chat.cooldown_s
        ));
    }
    if chat.channels.is_empty() {
        out.push("gameplay.chat.channels: не должно быть пустым".into());
    }
    let mut seen = HashSet::new();
    for ch in &chat.channels {
        if ch.trim().is_empty() {
            out.push("gameplay.chat.channels: пустой канал".into());
        }
        if !ch.trim().is_empty() && !seen.insert(ch.as_str()) {
            out.push(format!("gameplay.chat.channels: дубликат {ch:?}"));
        }
    }
}

fn check_inventory(inv: &InventorySection, out: &mut Vec<String>) {
    if inv.slots == 0 {
        out.push("gameplay.inventory.slots: должен быть ≥ 1".into());
    }
    if inv.equip_slots.is_empty() {
        out.push("gameplay.inventory.equip_slots: не должно быть пустым".into());
    }
    let mut seen = HashSet::new();
    for slot in &inv.equip_slots {
        if slot.trim().is_empty() {
            out.push("gameplay.inventory.equip_slots: пустой слот".into());
        }
        if !slot.trim().is_empty() && !seen.insert(slot.as_str()) {
            out.push(format!("gameplay.inventory.equip_slots: дубликат {slot:?}"));
        }
    }
}

/// `host:port` с непустым хостом и портом 0–65535; IPv6 — в квадратных скобках.
fn check_host_port(bind: &str) -> Result<(), String> {
    let (host, port) = bind
        .rsplit_once(':')
        .ok_or_else(|| "ожидается host:port".to_string())?;
    if host.is_empty() {
        return Err("хост пуст (ожидается host:port)".into());
    }
    if host.contains(':') && !host.starts_with('[') {
        return Err("IPv6-адрес — в квадратных скобках: [::1]:24817".into());
    }
    port.parse::<u16>()
        .map_err(|_| format!("порт {port:?} не число 0–65535"))?;
    Ok(())
}

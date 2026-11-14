//! Авторизация (ADR-006, D29/D30): argon2id-пароли, opaque-токены (в БД —
//! только sha256-хэш), rate-limit по IP и сборка `evt/session.start` (§5)
//! из `server.toml` (D25 — единственный источник лимитов/фич).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use ld2_protocol::features::{
    ChatConfig, CrimeConfig, CrimeKind as ProtocolCrimeKind, InventoryConfig, MagicConfig,
    ManaConfig, QuestsConfig, RefreshToken, ServerInfo, SessionConfig, SessionTokens,
    UpgradeConfig,
};
use ld2_protocol::types::{CharacterSummary, Severity};
use serde_json::{json, Value};

use crate::config::{Config, FeatureValue};

// ─── пароли (D30) ────────────────────────────────────────────────────────────

/// argon2id (дефолтные параметры argon2); соль — внутренняя, CSPRNG (PHC-строка
/// в `accounts.password_hash`).
pub fn hash_password(plain: &str) -> String {
    Argon2::default()
        .hash_password(plain.as_bytes())
        .expect("хэширование пароля")
        .to_string()
}

/// Проверка пароля; битый PHC в БД → отказ (не паника).
pub fn verify_password(plain: &str, phc: &str) -> bool {
    PasswordHash::new(phc)
        .map(|parsed| {
            Argon2::default()
                .verify_password(plain.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

// ─── email (D33, ADR-007) ────────────────────────────────────────────────────

/// Нормализация email: `trim` + lowercase — применяется перед вставью и
/// сравнением (UNIQUE действует по нормализованному значению, D33).
pub fn normalize_email(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// Синтаксис email (D33): непуст, ≤254, ровно один `@`, локальная часть
/// непуста, домен содержит точку и не начинается/не заканчивается ею,
/// без пробелов. Сервер — источник истины (D34), проверка намеренно
/// консервативная: форматную валидацию защищает от опечаток, а не
/// заменяет проверку почтового сервиса (MX — вне v1, ADR-007).
pub fn validate_email(normalized: &str) -> bool {
    if normalized.is_empty() || normalized.len() > 254 {
        return false;
    }
    if normalized
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    let Some((local, domain)) = normalized.split_once('@') else {
        return false;
    };
    // Ровно один '@': второй @ остаётся в домене → домен с '@' невалиден.
    if local.is_empty() || domain.contains('@') {
        return false;
    }
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return false;
    }
    if domain.contains("..") || local.ends_with('.') || local.starts_with('.') {
        return false;
    }
    true
}

// ─── токены (D29) ────────────────────────────────────────────────────────────

/// Новый opaque-токен: 32 байта CSPRNG → hex; второе значение — sha256-хэш
/// для БД (D30: в открытом виде в БД не хранится).
pub fn new_token() -> (String, String) {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("CSPRNG доступен на платформе");
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let hash = token_hash(&token);
    (token, hash)
}

/// sha256-токена в hex — ключ поиска в `refresh_tokens`.
pub fn token_hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// ─── rate-limit auth-команд по IP (D30) ─────────────────────────────────────

/// Фиксированное окно: не больше [`MAX_ATTEMPTS`] auth-команд в [`WINDOW_S`]
/// секунд с одного IP → `E_RATE_LIMITED` + `retry_after_s` (§9).
#[derive(Default)]
pub struct AuthRateLimiter {
    windows: Mutex<HashMap<IpAddr, (i64, u32)>>,
}

const WINDOW_S: i64 = 60;
const MAX_ATTEMPTS: u32 = 20;

impl AuthRateLimiter {
    pub fn new() -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// `None` — можно; `Some(retry_after_s)` — отклонить.
    pub fn check(&self, ip: IpAddr) -> Option<u64> {
        let now = crate::time::now_epoch();
        let mut windows = self.windows.lock().expect("rate limiter");
        windows.retain(|_, (start, _)| now.saturating_sub(*start) < WINDOW_S);
        let entry = windows.entry(ip).or_insert((now, 0));
        if now.saturating_sub(entry.0) >= WINDOW_S {
            *entry = (now, 0);
        }
        entry.1 += 1;
        if entry.1 > MAX_ATTEMPTS {
            Some((WINDOW_S - (now - entry.0)).max(1) as u64)
        } else {
            None
        }
    }
}

// ─── evt/session.start (§5) ─────────────────────────────────────────────────

/// Сборка `evt/session.start` **целиком из конфига** (D25): лимиты, фичи,
/// преступность, чат, инвентарь, улучшение, мана — без хардкода в коде.
pub fn session_config(
    cfg: &Config,
    access_token: String,
    refresh_token: String,
    characters: Vec<CharacterSummary>,
    // M14/F19: роль аккаунта из БД (модерация чата).
    role: String,
) -> SessionConfig {
    let auth = &cfg.auth;
    let g = &cfg.gameplay;
    SessionConfig {
        server: ServerInfo {
            name: cfg.server.name.clone(),
            region: Some(cfg.server.region.clone()),
            min_client: cfg.server.min_client.clone(),
        },
        session: SessionTokens {
            access_token,
            expires_in_s: auth.access_ttl_s,
            refresh: RefreshToken {
                token: refresh_token,
                expires_in_s: auth.refresh_ttl_s,
            },
        },
        char_slot_limit: g.char_slot_limit,
        profession_limit: g.profession_limit,
        role,
        features: g
            .features
            .iter()
            .map(|(k, v)| (k.clone(), feature_value(v)))
            .collect(),
        crime: CrimeConfig {
            t_max_s: g.crime.t_max_s as i64,
            blend_k: g.crime.blend_k,
            base: g
                .crime
                .base
                .iter()
                .map(|(k, v)| (k.clone(), *v as i64))
                .collect(),
            kinds: g
                .crime
                .kinds
                .iter()
                .map(|k| ProtocolCrimeKind {
                    kind: k.kind.clone(),
                    title: k.title.clone(),
                    severity: severity(&k.severity),
                    extra: Default::default(),
                })
                .collect(),
        },
        chat: ChatConfig {
            server_history: g.chat.server_history,
            channels: g.chat.channels.clone(),
            max_len: g.chat.max_len,
            cooldown_s: g.chat.cooldown_s,
        },
        inventory: InventoryConfig {
            slots: g.inventory.slots,
            equip_slots: g.inventory.equip_slots.clone(),
        },
        upgrade: UpgradeConfig {
            max_level: g.upgrade.max_level,
            preview: g.upgrade.preview,
            shows_chance: g.upgrade.shows_chance,
            duration_s: g.upgrade.duration_s,
        },
        magic: MagicConfig {
            rune_recharge: g.magic.rune_recharge.clone(),
            scroll_requires_target: g.magic.scroll_requires_target,
            mana: ManaConfig {
                regen_out_s: g.magic.mana.regen_out_s,
                regen_in_pct_s: g.magic.mana.regen_in_pct_s,
                rune_cost: g.magic.mana.rune_cost as i64,
            },
        },
        // M18/F36: лимит текущих квестов (§5, DoD: конфиг, не код).
        quests: QuestsConfig {
            max_active: g.quests.max_active,
        },
        characters,
        extra: Default::default(),
    }
}

/// Capability-флаг §5: булево/строка → JSON (открытые ключи разрешены).
fn feature_value(v: &FeatureValue) -> Value {
    match v {
        FeatureValue::Bool(b) => json!(b),
        FeatureValue::Str(s) => json!(s),
    }
}

/// Строка `severity` из конфига → enum §5; неизвестная → нейтральный UI (§2.3).
pub(crate) fn severity(s: &str) -> Severity {
    match s {
        "low" => Severity::Low,
        "medium" => Severity::Medium,
        "high" => Severity::High,
        _ => Severity::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_normalize_trims_and_lowercases() {
        assert_eq!(
            normalize_email("  Player@Example.COM "),
            "player@example.com"
        );
        assert_eq!(normalize_email("A@B.RU"), "a@b.ru");
    }

    #[test]
    fn email_validation_accepts_normal_addresses() {
        for ok in [
            "player@example.com",
            "a@b.co",
            "first.last+tag@sub.example.org",
            "пользователь@пример.рф", // IDN допустим по UTF-8 (сервер не транслитерирует)
        ] {
            assert!(validate_email(&normalize_email(ok)), "валиден: {ok}");
        }
    }

    #[test]
    fn email_validation_rejects_broken_addresses() {
        for bad in [
            "",
            "   ",
            "no-at-sign.example.com",
            "@example.com",
            "player@",
            "two@@example.com",
            "player@example", // домен без точки
            "player@.example.com",
            "player@example.",
            "player@example..com",
            "play er@example.com", // пробел
        ] {
            let normalized = normalize_email(bad);
            assert!(!validate_email(&normalized), "невалиден: {bad:?}");
        }
        // Длина >254.
        let too_long = format!("{}@e.com", "x".repeat(250));
        assert!(!validate_email(&normalize_email(&too_long)), "длина >254");
    }

    #[test]
    fn password_hash_round_trip() {
        let phc = hash_password("longenough1");
        assert!(phc.starts_with("$argon2"), "PHC-строка argon2id: {phc}");
        assert!(verify_password("longenough1", &phc));
        assert!(!verify_password("wrong", &phc));
        assert!(
            !verify_password("longenough1", "не-хэш"),
            "битый PHC — отказ"
        );
    }

    #[test]
    fn tokens_are_opaque_and_hashed() {
        let (t1, h1) = new_token();
        let (t2, h2) = new_token();
        assert_eq!(t1.len(), 64, "32 байта в hex");
        assert_ne!(t1, t2, "токены уникальны");
        assert_ne!(t1, h1, "в паре — хэш, не сам токен");
        assert_eq!(h1, token_hash(&t1), "хэш детерминирован");
        assert_ne!(h1, h2);
    }

    #[test]
    fn rate_limiter_throttles_after_burst() {
        let limiter = AuthRateLimiter::new();
        let ip: IpAddr = "198.51.100.7".parse().expect("ip");
        for _ in 0..MAX_ATTEMPTS {
            assert_eq!(limiter.check(ip), None, "в пределах окна пропускаем");
        }
        let retry = limiter.check(ip).expect("21-я попытка должна отклоняться");
        assert!(
            (1..=WINDOW_S as u64).contains(&retry),
            "retry_after_s: {retry}"
        );
        // другой IP не страдает
        let other: IpAddr = "203.0.113.9".parse().expect("ip");
        assert_eq!(limiter.check(other), None);
    }
}

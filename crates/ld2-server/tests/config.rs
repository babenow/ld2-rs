//! Валидация `server.toml` (M12.1): fail-fast с указанием ключа (D25).
//!
//! Негативные кейсы строятся правкой шаблона `server.example.toml` — так
//! тесты гарантированно следуют за реальным конфигом, а не за его копией.

use std::path::PathBuf;

use ld2_server::config::Config;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn example_path() -> PathBuf {
    crate_dir().join("server.example.toml")
}

fn example_text() -> String {
    std::fs::read_to_string(example_path()).expect("server.example.toml читается")
}

/// Правит фрагмент шаблона; паникует, если фрагмента нет (защита от
/// «затихшего» теста при рефакторинге конфига).
fn edited(old: &str, new: &str) -> String {
    let text = example_text();
    assert!(text.contains(old), "в шаблоне нет фрагмента {old:?}");
    text.replacen(old, new, 1)
}

fn problems_of(text: &str) -> Vec<String> {
    let cfg: Config = toml::from_str(text).expect("TOML должен парситься");
    cfg.problems()
}

fn assert_problem(text: &str, key: &str) {
    let problems = problems_of(text);
    assert!(
        problems.iter().any(|p| p.contains(key)),
        "ожидалась проблема с ключом {key:?}, получено: {problems:#?}"
    );
}

#[test]
fn example_is_valid() {
    Config::load(example_path()).expect("server.example.toml должен проходить валидацию");
}

#[test]
fn missing_file_is_io_error() {
    let err =
        Config::load(crate_dir().join("нет-такого-файла.toml")).expect_err("файла нет — ошибка");
    assert!(err.to_string().contains("нет-такого-файла.toml"), "{err}");
}

#[test]
fn missing_key_is_reported() {
    let text = edited("bind = \"0.0.0.0:24817\"", "# bind удалён для теста");
    let err = toml::from_str::<Config>(&text).expect_err("bind обязателен");
    assert!(err.to_string().contains("bind"), "сообщение: {err}");
}

#[test]
fn unknown_key_is_rejected() {
    let text = edited("region = \"ru\"", "region = \"ru\"\nbogus = 1");
    let err = toml::from_str::<Config>(&text).expect_err("неизвестный ключ должен отклоняться");
    assert!(err.to_string().contains("bogus"), "сообщение: {err}");
}

#[test]
fn bad_bind_port_names_key() {
    assert_problem(
        &edited("bind = \"0.0.0.0:24817\"", "bind = \"0.0.0.0:http\""),
        "server.bind",
    );
}

#[test]
fn bind_without_port_names_key() {
    assert_problem(
        &edited("bind = \"0.0.0.0:24817\"", "bind = \"localhost\""),
        "server.bind",
    );
}

#[test]
fn bad_log_level_names_key() {
    // "info,foo=bar": level `bar` невалиден → ошибка EnvFilter
    assert_problem(
        &edited("log_level = \"info\"", "log_level = \"info,foo=bar\""),
        "server.log_level",
    );
}

#[test]
fn bad_database_scheme_names_key() {
    let text = edited("url = \"postgres://", "url = \"http://");
    assert_problem(&text, "database.url");
}

#[test]
fn refresh_not_longer_than_access() {
    assert_problem(
        &edited("refresh_ttl_s = 2592000", "refresh_ttl_s = 300"),
        "auth.refresh_ttl_s",
    );
}

#[test]
fn zero_char_slots_names_key() {
    assert_problem(
        &edited("char_slot_limit = 8", "char_slot_limit = 0"),
        "gameplay.char_slot_limit",
    );
}

/// M18/F36: нулевой лимит текущих квестов — ошибка валидации.
#[test]
fn zero_quest_max_active_names_key() {
    assert_problem(
        &edited("max_active = 10", "max_active = 0"),
        "gameplay.quests.max_active",
    );
}

/// M18/F39: наценка сервера меньше 1 — ошибка валидации.
#[test]
fn bad_shop_markup_names_key() {
    assert_problem(
        &edited("server_markup = 1.0", "server_markup = 0.5"),
        "gameplay.shop.server_markup",
    );
}

/// M18/F39: потолок скидки вне 0..=10000 — ошибка валидации.
#[test]
fn shop_max_discount_out_of_range_names_key() {
    assert_problem(
        &edited("max_discount_bp = 5000", "max_discount_bp = 20000"),
        "gameplay.shop.max_discount_bp",
    );
}

#[test]
fn feature_wrong_type_names_key() {
    // строка вместо bool: untagged-парсинг проходит, ловит валидация
    let text = edited("\"chat.slash\" = true", "\"chat.slash\" = \"yes\"");
    assert_problem(&text, "gameplay.features.chat.slash");
}

#[test]
fn empty_crime_kinds_names_key() {
    // шаблон описывает kinds через [[…]]-массив, поэтому вырезаем блоки и
    // вставляем явно пустой список в секцию [gameplay.crime]
    let text = example_text();
    let start = text
        .find("[[gameplay.crime.kinds]]")
        .expect("в шаблоне есть kinds");
    let end = text
        .find("\n[gameplay.chat]")
        .expect("после kinds идёт chat");
    let mut trimmed = format!("{}{}", &text[..start], &text[end..]);
    trimmed = trimmed.replacen("t_max_s = 604800", "t_max_s = 604800\nkinds = []", 1);
    assert_problem(&trimmed, "gameplay.crime.kinds");
}

#[test]
fn duplicate_crime_kind_names_key() {
    assert_problem(
        &edited("kind = \"thief\"", "kind = \"murderer\""),
        "gameplay.crime.kinds",
    );
}

#[test]
fn negative_chat_cooldown_names_key() {
    assert_problem(
        &edited("cooldown_s = 1.5", "cooldown_s = -1"),
        "gameplay.chat.cooldown_s",
    );
}

#[test]
fn zero_tick_hz_names_key() {
    assert_problem(&edited("tick_hz = 10", "tick_hz = 0"), "world.tick_hz");
}

#[test]
fn collects_all_problems_at_once() {
    let text = edited("bind = \"0.0.0.0:24817\"", "bind = \"nope\"")
        .replace("char_slot_limit = 8", "char_slot_limit = 0");
    let problems = problems_of(&text);
    assert!(
        problems.iter().any(|p| p.contains("server.bind")),
        "{problems:#?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("gameplay.char_slot_limit")),
        "{problems:#?}"
    );
    assert!(
        problems.len() >= 2,
        "обе проблемы должны быть собраны: {problems:#?}"
    );
}

/// `deploy/server.toml` (Docker Compose) — дубль шаблона с другим хостом БД;
/// тест не даёт файлам разойтись по составу ключей.
#[test]
fn deploy_config_matches_example_except_db_url() {
    fn meaningful(text: &str) -> Vec<&str> {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    }

    let example_text = example_text();
    let example = meaningful(&example_text);
    let deploy_path = crate_dir().join("../../deploy/server.toml");
    let deploy_text = std::fs::read_to_string(&deploy_path).expect("deploy/server.toml читается");
    Config::load(&deploy_path).expect("deploy/server.toml должен проходить валидацию");
    let deploy = meaningful(&deploy_text);

    assert_eq!(
        example.len(),
        deploy.len(),
        "deploy/server.toml разошёлся со шаблоном по составу ключей"
    );
    let diffs: Vec<(&str, &str)> = example
        .iter()
        .zip(deploy.iter())
        .filter(|(e, d)| e != d)
        .map(|(e, d)| (*e, *d))
        .collect();
    assert_eq!(
        diffs.len(),
        1,
        "расхождение только в database.url: {diffs:?}"
    );
    assert!(
        diffs[0].0.starts_with("url = "),
        "в шаблоне это url: {:?}",
        diffs[0].0
    );
    assert!(
        diffs[0].1.starts_with("url = "),
        "в deploy это url: {:?}",
        diffs[0].1
    );
}

//! RF0a: полнота `dispatch` — каждая команда реестра обрабатывается, и каждая
//! ветка `dispatch` зарегистрирована в протоколе.
//!
//! Тест статический: читает исходник `session.rs` (`include_str!`) и сверяет
//! литералы `"cmd/..."` в теле `fn dispatch` с `ld2_protocol::cmd::ALL_COMMANDS`.
//! Ни БД, ни сервера не нужно, а забытая ветка или опечатка в строке ловится на
//! `cargo test` — до того, как команда молча провалится в catch-all
//! `"неизвестная команда"` (E_INTERNAL).

use ld2_protocol::cmd::ALL_COMMANDS;

/// Тело `async fn dispatch` из `session.rs` (границы по балансу скобок).
fn dispatch_body() -> &'static str {
    let src = include_str!("../src/session/mod.rs");
    let start = src
        .find("async fn dispatch")
        .expect("dispatch в session.rs");
    let open = start + src[start..].find('{').expect("тело dispatch");
    let bytes = src.as_bytes();
    let (mut depth, mut i) = (0usize, open);
    let (mut in_str, mut in_line, mut in_block) = (false, false, false);
    while i < bytes.len() {
        let c = bytes[i] as char;
        let next = bytes.get(i + 1).map(|b| *b as char);
        if in_line {
            if c == '\n' {
                in_line = false;
            }
            i += 1;
            continue;
        }
        if in_block {
            if c == '*' && next == Some('/') {
                in_block = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if in_str {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => in_str = true,
            '/' if next == Some('/') => {
                in_line = true;
                i += 2;
                continue;
            }
            '/' if next == Some('*') => {
                in_block = true;
                i += 2;
                continue;
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open..=i];
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("незакрытое тело dispatch");
}

/// Имя константы `ld2_protocol::names` для wire-команды (та же схема,
/// что в `names.rs`: `CMD_` + хвост после `/`, не-алфанумерика → `_`).
fn const_name(wire: &str) -> String {
    let tail = wire.split_once('/').map(|(_, tail)| tail).unwrap_or(wire);
    let mut out = String::from("CMD_");
    for ch in tail.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    out
}

/// Уникальные `names::CMD_*` в теле `dispatch` — после M38 wire-строки
/// заменены константами протокола.
fn dispatch_commands() -> Vec<String> {
    let body = dispatch_body();
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(pos) = rest.find("names::CMD_") {
        let tail = &rest[pos + "names::CMD_".len()..];
        let end = tail
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(tail.len());
        let name = format!("CMD_{}", &tail[..end]);
        if !out.contains(&name) {
            out.push(name);
        }
        rest = &tail[end..];
    }
    out
}

#[test]
fn dispatch_covers_every_registered_command() {
    let registered: Vec<(String, String)> = ALL_COMMANDS
        .iter()
        .map(|c| (c.name.to_string(), const_name(c.name)))
        .collect();
    let handled = dispatch_commands();
    assert!(
        !handled.is_empty(),
        "в dispatch не найдено ни одной команды"
    );

    for (wire, name) in &registered {
        assert!(
            handled.contains(name),
            "команда `{wire}` ({name}) есть в ALL_COMMANDS, но не в dispatch — уйдёт в catch-all"
        );
    }
    let known: Vec<&String> = registered.iter().map(|(_, name)| name).collect();
    for name in &handled {
        assert!(
            known.contains(&name),
            "dispatch обрабатывает `{name}`, которой нет в ALL_COMMANDS/protocol.md §7"
        );
    }
    assert_eq!(
        handled.len(),
        registered.len(),
        "число веток dispatch и записей реестра"
    );
}

//! Горячие клавиши (ФР-19: ремаппинг): спецификация вида `alt+up` / `tab`.
//!
//! Спецификации живут в [`crate::config::Hotkeys`] (персистентны в `config.toml`),
//! здесь — разбор/сборка и «запись» нажатия для UI настроек.

use eframe::egui;

/// Нажата ли спецификация в текущем вводе.
///
/// Модификаторы должны совпадать ровно (чтобы `alt+up` не срабатывал от
/// `ctrl+alt+up`), клавиша — `key_pressed`.
pub fn pressed(input: &egui::InputState, spec: &str) -> bool {
    let Some(parsed) = parse(spec) else {
        return false;
    };
    let m = &input.modifiers;
    m.alt == parsed.alt
        && m.ctrl == parsed.ctrl
        && m.shift == parsed.shift
        && m.command == parsed.cmd
        && input.key_pressed(parsed.key)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Parsed {
    alt: bool,
    ctrl: bool,
    shift: bool,
    cmd: bool,
    key: egui::Key,
}

/// Разбор спецификации `mod+mod+key` (клавиша — последняя часть).
fn parse(spec: &str) -> Option<Parsed> {
    let mut p = Parsed {
        alt: false,
        ctrl: false,
        shift: false,
        cmd: false,
        key: egui::Key::Tab,
    };
    let mut parts: Vec<&str> = spec.split('+').collect();
    let name = parts.pop()?.trim().to_ascii_lowercase();
    p.key = key_from_name(&name)?;
    for part in parts {
        match part.trim().to_ascii_lowercase().as_str() {
            "alt" => p.alt = true,
            "ctrl" | "control" => p.ctrl = true,
            "shift" => p.shift = true,
            "cmd" | "command" | "meta" => p.cmd = true,
            _ => return None,
        }
    }
    Some(p)
}

/// Имя клавиши → `egui::Key` (основной набор действий + буквенно-цифровые).
fn key_from_name(name: &str) -> Option<egui::Key> {
    Some(match name {
        "up" => egui::Key::ArrowUp,
        "down" => egui::Key::ArrowDown,
        "left" => egui::Key::ArrowLeft,
        "right" => egui::Key::ArrowRight,
        "tab" => egui::Key::Tab,
        "enter" | "return" => egui::Key::Enter,
        "space" => egui::Key::Space,
        "escape" | "esc" => egui::Key::Escape,
        "backspace" => egui::Key::Backspace,
        "delete" => egui::Key::Delete,
        "home" => egui::Key::Home,
        "end" => egui::Key::End,
        "pageup" => egui::Key::PageUp,
        "pagedown" => egui::Key::PageDown,
        "f1" => egui::Key::F1,
        "f2" => egui::Key::F2,
        "f3" => egui::Key::F3,
        "f4" => egui::Key::F4,
        "f5" => egui::Key::F5,
        "f6" => egui::Key::F6,
        "f7" => egui::Key::F7,
        "f8" => egui::Key::F8,
        "f9" => egui::Key::F9,
        "f10" => egui::Key::F10,
        "f11" => egui::Key::F11,
        "f12" => egui::Key::F12,
        "a" => egui::Key::A,
        "b" => egui::Key::B,
        "c" => egui::Key::C,
        "d" => egui::Key::D,
        "e" => egui::Key::E,
        "f" => egui::Key::F,
        "g" => egui::Key::G,
        "h" => egui::Key::H,
        "i" => egui::Key::I,
        "j" => egui::Key::J,
        "k" => egui::Key::K,
        "l" => egui::Key::L,
        "m" => egui::Key::M,
        "n" => egui::Key::N,
        "o" => egui::Key::O,
        "p" => egui::Key::P,
        "q" => egui::Key::Q,
        "r" => egui::Key::R,
        "s" => egui::Key::S,
        "t" => egui::Key::T,
        "u" => egui::Key::U,
        "v" => egui::Key::V,
        "w" => egui::Key::W,
        "x" => egui::Key::X,
        "y" => egui::Key::Y,
        "z" => egui::Key::Z,
        "0" => egui::Key::Num0,
        "1" => egui::Key::Num1,
        "2" => egui::Key::Num2,
        "3" => egui::Key::Num3,
        "4" => egui::Key::Num4,
        "5" => egui::Key::Num5,
        "6" => egui::Key::Num6,
        "7" => egui::Key::Num7,
        "8" => egui::Key::Num8,
        "9" => egui::Key::Num9,
        _ => return None,
    })
}

/// Имя клавиши из `egui::Key` (для обратной сборки спецификации).
fn name_from_key(key: egui::Key) -> Option<&'static str> {
    Some(match key {
        egui::Key::ArrowUp => "up",
        egui::Key::ArrowDown => "down",
        egui::Key::ArrowLeft => "left",
        egui::Key::ArrowRight => "right",
        egui::Key::Tab => "tab",
        egui::Key::Enter => "enter",
        egui::Key::Space => "space",
        egui::Key::Escape => "escape",
        egui::Key::Backspace => "backspace",
        egui::Key::Delete => "delete",
        egui::Key::Home => "home",
        egui::Key::End => "end",
        egui::Key::PageUp => "pageup",
        egui::Key::PageDown => "pagedown",
        egui::Key::F1 => "f1",
        egui::Key::F2 => "f2",
        egui::Key::F3 => "f3",
        egui::Key::F4 => "f4",
        egui::Key::F5 => "f5",
        egui::Key::F6 => "f6",
        egui::Key::F7 => "f7",
        egui::Key::F8 => "f8",
        egui::Key::F9 => "f9",
        egui::Key::F10 => "f10",
        egui::Key::F11 => "f11",
        egui::Key::F12 => "f12",
        egui::Key::A => "a",
        egui::Key::B => "b",
        egui::Key::C => "c",
        egui::Key::D => "d",
        egui::Key::E => "e",
        egui::Key::F => "f",
        egui::Key::G => "g",
        egui::Key::H => "h",
        egui::Key::I => "i",
        egui::Key::J => "j",
        egui::Key::K => "k",
        egui::Key::L => "l",
        egui::Key::M => "m",
        egui::Key::N => "n",
        egui::Key::O => "o",
        egui::Key::P => "p",
        egui::Key::Q => "q",
        egui::Key::R => "r",
        egui::Key::S => "s",
        egui::Key::T => "t",
        egui::Key::U => "u",
        egui::Key::V => "v",
        egui::Key::W => "w",
        egui::Key::X => "x",
        egui::Key::Y => "y",
        egui::Key::Z => "z",
        egui::Key::Num0 => "0",
        egui::Key::Num1 => "1",
        egui::Key::Num2 => "2",
        egui::Key::Num3 => "3",
        egui::Key::Num4 => "4",
        egui::Key::Num5 => "5",
        egui::Key::Num6 => "6",
        egui::Key::Num7 => "7",
        egui::Key::Num8 => "8",
        egui::Key::Num9 => "9",
        _ => return None,
    })
}

/// Ловит первое нажатие (не-модификатор) в вводе → спецификация для
/// «записи» в настройках (ФР-19). Модификаторы берутся из `modifiers`
/// события — итог `alt+up` собирается корректно.
pub fn spec_from_input(input: &egui::InputState) -> Option<String> {
    for event in &input.events {
        let egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            repeat,
            ..
        } = event
        else {
            continue;
        };
        if *repeat {
            continue;
        }
        // Нераспознанные клавиши (в т.ч. модификаторы) — пропускаем.
        let Some(name) = name_from_key(*key) else {
            continue;
        };
        let mut spec = String::new();
        if modifiers.alt {
            spec.push_str("alt+");
        }
        if modifiers.ctrl {
            spec.push_str("ctrl+");
        }
        if modifiers.shift {
            spec.push_str("shift+");
        }
        if modifiers.command {
            spec.push_str("cmd+");
        }
        spec.push_str(name);
        return Some(spec);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_specs_round_trip() {
        for spec in ["alt+up", "tab", "ctrl+shift+f5", "alt+left", "a"] {
            let p = parse(spec).expect(spec);
            let name = name_from_key(p.key).expect("имя");
            let mut rebuilt = String::new();
            if p.alt {
                rebuilt.push_str("alt+");
            }
            if p.ctrl {
                rebuilt.push_str("ctrl+");
            }
            if p.shift {
                rebuilt.push_str("shift+");
            }
            if p.cmd {
                rebuilt.push_str("cmd+");
            }
            rebuilt.push_str(name);
            assert_eq!(rebuilt, spec, "round-trip {spec}");
        }
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse("").is_none());
        assert!(parse("hyper+up").is_none(), "неизвестный модификатор");
        assert!(parse("bracketleft").is_none(), "неизвестная клавиша");
    }

    #[test]
    fn default_hotkeys_parse() {
        let hk = crate::config::Hotkeys::default();
        for spec in [
            hk.nav_up.as_str(),
            hk.nav_down.as_str(),
            hk.nav_left.as_str(),
            hk.nav_right.as_str(),
            hk.cycle_target.as_str(),
        ] {
            assert!(parse(spec).is_some(), "дефолт '{spec}' должен разбираться");
        }
    }
}

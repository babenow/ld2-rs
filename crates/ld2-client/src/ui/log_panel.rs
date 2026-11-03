//! Панель журнала мира (ФР-17): цвета — палитра §7.4 ТЗ, автопрокрутка
//! вниз. Состояние — в [`crate::store::AppStore::log`] (store без egui).

use std::collections::VecDeque;

use eframe::egui;

use crate::l10n::{t, Key, Lang};
use crate::store::{LogEntry, LogKind};

/// Палитра §7.4 (контрастная к тёмной теме).
fn color(kind: LogKind) -> egui::Color32 {
    use LogKind::*;
    match kind {
        System => egui::Color32::from_rgb(0x7F, 0x84, 0x8E),
        Location => egui::Color32::from_rgb(0xAB, 0xB2, 0xBF),
        CombatOut => egui::Color32::from_rgb(0xE0, 0x6C, 0x75), // исходящий урон
        CombatIn => egui::Color32::from_rgb(0x98, 0xC3, 0x79),  // входящий
        Magic => egui::Color32::from_rgb(0x61, 0xAF, 0xEF),
        Loot => egui::Color32::from_rgb(0xE5, 0xC0, 0x7B),
        Crime => egui::Color32::from_rgb(0xD1, 0x9A, 0x66),
        Quest => egui::Color32::from_rgb(0xC6, 0x78, 0xDD),
        Xp => egui::Color32::from_rgb(0x56, 0xB6, 0xC2),
        Chat => egui::Color32::from_rgb(0xEB, 0xEB, 0xEB),
    }
}

pub fn show(ui: &mut egui::Ui, lang: Lang, log: &VecDeque<LogEntry>) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.strong(t(lang, Key::LogLabel));
            ui.weak(format!("{} строк", log.len()));
        });
        egui::ScrollArea::vertical()
            .max_height(150.0)
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if log.is_empty() {
                    ui.weak("…");
                }
                for entry in log {
                    ui.colored_label(color(entry.kind), &entry.text);
                }
            });
    });
}

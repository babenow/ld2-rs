//! Нижняя панель чата (ФР-13): вкладки шести каналов, фильтр-строка,
//! история (stick-to-bottom), ввод со слэш-командами (§5 `chat.slash`).
//!
//! Состояние истории — в [`crate::store::AppStore::chat`] (store без egui),
//! ввод/фильтр — в [`crate::ui::Forms::chat`] (живут между кадрами).

use eframe::egui;
use ld2_protocol::cmd::ChatSend;

use crate::l10n::{t, tf, Key};
use crate::store::{parse_chat_input, Act, ChatDraft, CHAT_CHANNELS};

/// Цветовая метка канала (палитра §7.4).
fn channel_color(channel: &str) -> egui::Color32 {
    match channel {
        "system" => egui::Color32::from_rgb(0x7F, 0x84, 0x8E),
        "global" => egui::Color32::from_rgb(0x61, 0xAF, 0xEF),
        "party" => egui::Color32::from_rgb(0x98, 0xC3, 0x79),
        "guild" => egui::Color32::from_rgb(0xC6, 0x78, 0xDD),
        "whisper" => egui::Color32::from_rgb(0xE0, 0x6C, 0x75),
        _ => egui::Color32::from_rgb(0xAB, 0xB2, 0xBF), // location
    }
}

/// Локализованное название канала (вкладки).
fn channel_key(channel: &str) -> Key {
    match channel {
        "system" => Key::ChSystem,
        "global" => Key::ChGlobal,
        "party" => Key::ChParty,
        "guild" => Key::ChGuild,
        "whisper" => Key::ChWhisper,
        _ => Key::ChLocation,
    }
}

pub fn show(ui: &mut egui::Ui, ctx: &mut crate::ui::UiCtx<'_>) {
    let lang = ctx.lang;
    ui.group(|ui| {
        // ── вкладки каналов + фильтр-строка (ФР-13) ──
        ui.horizontal(|ui| {
            let active = ctx.store.chat_channel.clone();
            for channel in CHAT_CHANNELS {
                let color = channel_color(channel);
                let selected = active == channel;
                let label = format!(
                    "{} ({})",
                    t(lang, channel_key(channel)),
                    ctx.store.chat.get(channel).map(|h| h.len()).unwrap_or(0)
                );
                if ui
                    .selectable_label(selected, egui::RichText::new(label).color(color))
                    .clicked()
                {
                    ctx.store.chat_channel = channel.to_string();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let filter = ui.add(
                    egui::TextEdit::singleline(&mut ctx.forms.chat.filter)
                        .hint_text(t(lang, Key::ChFilter))
                        .desired_width(120.0),
                );
                if filter.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    ctx.forms.chat.filter.clear();
                }
            });
        });

        // ── история активного канала (ring, stick-to-bottom) ──
        let channel = ctx.store.chat_channel.clone();
        let filter = ctx.forms.chat.filter.to_lowercase();
        let messages: Vec<String> = ctx
            .store
            .chat
            .get(&channel)
            .map(|hist| {
                hist.iter()
                    .filter(|m| {
                        filter.is_empty()
                            || m.text.to_lowercase().contains(&filter)
                            || m.name.to_lowercase().contains(&filter)
                    })
                    .map(|m| format!("{}: {}", m.name, m.text))
                    .collect()
            })
            .unwrap_or_default();
        egui::ScrollArea::vertical()
            .max_height(130.0)
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if messages.is_empty() {
                    ui.weak(t(lang, Key::ChEmpty));
                }
                let color = channel_color(&channel);
                for line in messages {
                    ui.colored_label(color, line);
                }
            });

        ui.separator();

        // ── ввод: Enter/кнопка отправки, слэш-команды (ФР-13) ──
        ui.horizontal(|ui| {
            let input = ui.add(
                egui::TextEdit::singleline(&mut ctx.forms.chat.input)
                    .desired_width(ui.available_width() - 170.0)
                    .hint_text("/w Имя, /p, /g …"),
            );
            let enter = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

            // Кнопка «ответить» последнему адресату whisper (ФР-13).
            if let Some(to) = ctx.store.last_whisper_from.clone() {
                if ui.small_button(t(lang, Key::ChReply)).clicked() {
                    ctx.forms.chat.input = format!("/w {to} ");
                    input.request_focus();
                }
            }
            if ui.button(t(lang, Key::ChSend)).clicked() || enter {
                send(ctx);
            }
        });
    });
}

/// Разбор ввода и отправка `cmd/chat.send` (или тост об ошибке команды).
fn send(ctx: &mut crate::ui::UiCtx<'_>) {
    let text = std::mem::take(&mut ctx.forms.chat.input);
    if text.trim().is_empty() {
        return;
    }
    let channel = ctx.store.chat_channel.clone();
    let reply_to = ctx.store.last_whisper_from.clone();
    match parse_chat_input(&text, &channel, reply_to.as_deref()) {
        ChatDraft::Send { channel, to, text } => {
            let cmd = ChatSend { channel, text, to };
            match ctx.transport.send(&cmd) {
                Ok(_) => ctx.store.on_sheet_sent(Act::Chat),
                Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
            }
        }
        ChatDraft::Unknown(cmd) => {
            // ФР-13: неизвестная команда — тост, в чат не отправляется.
            ctx.store.notice = Some(tf(ctx.lang, Key::ChUnknownCmd, &[("{cmd}", cmd.as_str())]));
        }
    }
}

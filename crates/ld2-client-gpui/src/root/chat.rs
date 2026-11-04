//! Панель чата gpui (M10.5) — порт `ui/chat_panel.rs` egui-клиента:
//! вкладки шести каналов (§5), история с фильтром, ввод со слэш-командами
//! (ФР-13), «ответить» последнему адресату whisper.

use gpui::{div, prelude::*, px, rgb, Context, Div};
use ld2_core::l10n::{t, tf, Key};
use ld2_core::store::{parse_chat_input, Act, ChatDraft, CHAT_CHANNELS};
use ld2_protocol::cmd::ChatSend;

use crate::palette::{palette, Palette};

use super::{act_btn, Root};

impl Root {
    pub(crate) fn chat_panel(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let dark = pal.bg == palette("dark").bg;
        let active = self.chat_channel.clone();
        let filter = self.chat_filter.read(cx).content.to_string().to_lowercase();
        let reply_to = self.store.last_whisper_from.clone();

        let mut panel = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(pal.border));

        // ── вкладки каналов + фильтр (ФР-13) ──
        let mut tabs = div().flex().flex_row().flex_wrap().gap_1().items_center();
        for channel in CHAT_CHANNELS {
            let count = self.store.chat.get(channel).map(|h| h.len()).unwrap_or(0);
            let selected = active == channel;
            let label = format!("{} ({count})", t(lang, channel_key(channel)));
            let color = channel_color(channel, dark);
            let ch = channel.to_string();
            // Выбранная вкладка — акцент, цвет канала — на тексте.
            let (bg, fg) = if selected {
                (pal.accent, pal.accent_text)
            } else {
                (pal.panel, color)
            };
            tabs = tabs.child(act_btn(cx, label, bg, fg, move |this, cx| {
                this.chat_channel = ch.clone();
                let _ = cx;
            }));
        }
        tabs = tabs.child(div().flex_grow_1().child(self.chat_filter.clone()));
        panel = panel.child(tabs);

        // ── история активного канала с фильтром ──
        let messages: Vec<String> = self
            .store
            .chat
            .get(&active)
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
        let color = channel_color(&active, dark);
        let mut history = div()
            .id("chat_history")
            .h(px(120.0))
            .overflow_y_scroll()
            .flex_shrink_0();
        if messages.is_empty() {
            history = history.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::ChEmpty)),
            );
        }
        for line in messages {
            history = history.child(div().text_size(px(12.0)).text_color(rgb(color)).child(line));
        }
        panel = panel.child(history);

        // ── ввод: Enter/кнопка, слэш-команды (ФР-13) ──
        let mut input_row = div().flex().flex_row().gap_1().items_center();
        input_row = input_row.child(
            div()
                .flex_grow_1()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(pal.border))
                .bg(rgb(pal.bg))
                .child(self.chat_input.clone()),
        );
        if reply_to.is_some() {
            input_row = input_row.child(act_btn(
                cx,
                t(lang, Key::ChReply),
                pal.panel,
                pal.text,
                |this, cx| {
                    if let Some(to) = this.store.last_whisper_from.clone() {
                        let text = format!("/w {to} ");
                        this.chat_input.update(cx, |i, _| i.content = text.into());
                    }
                },
            ));
        }
        input_row = input_row.child(act_btn(
            cx,
            t(lang, Key::ChSend),
            pal.accent,
            pal.accent_text,
            |this, cx| this.send_chat(cx),
        ));
        panel = panel.child(input_row);
        panel
    }

    /// Отправка ввода чата: слэш-команды парсятся, неизвестная — тост
    /// и в чат НЕ уходит (ФР-13).
    pub(crate) fn send_chat(&mut self, cx: &mut Context<'_, Self>) {
        let text = self.chat_input.read(cx).content.to_string();
        if text.trim().is_empty() {
            return;
        }
        self.chat_input.update(cx, |i, _| i.content = "".into());
        let channel = self.chat_channel.clone();
        let reply = self.store.last_whisper_from.clone();
        match parse_chat_input(&text, &channel, reply.as_deref()) {
            ChatDraft::Send { channel, to, text } => {
                let cmd = ChatSend { channel, text, to };
                match self.transport.send(&cmd) {
                    Ok(_) => self.store.on_sheet_sent(Act::Chat),
                    Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
                }
            }
            ChatDraft::Unknown(cmd) => {
                self.store.notice =
                    Some(tf(self.lang, Key::ChUnknownCmd, &[("{cmd}", cmd.as_str())]));
            }
        }
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

/// Цвет канала (палитра §7.4; светлая тема — затемнение светлых).
fn channel_color(channel: &str, dark: bool) -> u32 {
    if !dark {
        return match channel {
            "system" => 0x5a_5e_66,
            "location" => 0x4a_4e_58,
            _ => channel_color(channel, true),
        };
    }
    match channel {
        "system" => 0x7f_84_8e,
        "global" => 0x61_af_ef,
        "party" => 0x98_c3_79,
        "guild" => 0xc6_78_dd,
        "whisper" => 0xe0_6c_75,
        _ => 0xab_b2_bf, // location
    }
}

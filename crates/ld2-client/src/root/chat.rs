//! Тело чата gpui (M10.5, M14/F7 — вкладка «Чат» блока журнала) — порт
//! `ui/chat_panel.rs` egui-клиента:
//! вкладки шести каналов (§5), история с фильтром, ввод со слэш-командами
//! (ФР-13), «ответить» последнему адресату whisper.

use gpui::{div, prelude::*, px, rgb, Context, Div};
use ld2_core::l10n::{t, tf, Key};
use ld2_core::store::{parse_chat_input, Act, ChatDraft, CHAT_CHANNELS};
use ld2_protocol::cmd::{ChatDelete, ChatSend};

use crate::palette::{palette, Palette};
use ely::buttons::IconButton;
use ely::forms::{Choice, Input, SearchInput};
use ely::layout::ScrollArea;
use ely::navigation::Tabs;
use ely::primitives::IconName;

use super::{act_btn, Root};

impl Root {
    /// M14/F7: тело чата **внутри блока журнала** (вкладка «Чат»):
    /// рамку и шапку даёт журнал-блок, здесь — каналы, фильтр,
    /// история и ввод.
    pub(crate) fn chat_body(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let dark = pal.bg == palette("dark").bg;
        let active = self.chat_channel.clone();
        let filter = self.ti(&self.chat_filter).read(cx).text().to_lowercase();
        let reply_to = self.store.last_whisper_from.clone();

        // M14/fix: панель тянется на остаток блока журнала — история
        // забирает свободное место, строка ввода прижата к низу чата
        // (без flex_grow_1 панель жила по контенту и ввод «висел»).
        let mut panel = div().flex().flex_col().gap_1().flex_grow_1().min_h_0();

        // ── вкладки каналов (ФР-13): Tabs Ely + фильтр ──
        let choices: Vec<Choice> = CHAT_CHANNELS
            .iter()
            .map(|channel| {
                let count = self.store.chat.get(*channel).map(|h| h.len()).unwrap_or(0);
                Choice::new(
                    *channel,
                    format!("{} ({count})", t(lang, channel_key(channel))),
                )
            })
            .collect();
        let this = cx.entity();
        let mut tabs = div().flex().flex_row().gap_2().items_center();
        tabs = tabs.child(
            Tabs::new("ld2-chat-channels", choices, active.clone()).on_change(
                move |value, _window, acx| {
                    let value = value.to_string();
                    this.update(acx, |root, cx| {
                        root.chat_channel = value;
                        cx.notify();
                    });
                },
            ),
        );
        panel = panel.child(tabs);
        // M14/F2: фильтр — отдельной строкой: в узкой колонке (чат
        // теперь внизу слева) одна строка под вкладки и поиск не влезает.
        // M14/F18: строка фильтра БЕЗ flex_grow_1 — в flex-колонке она
        // забирала вертикаль и толкала историю вниз (сообщения
        // «висели по центру»); теперь история занимает остаток сверху.
        panel = panel.child(SearchInput::new(
            "ld2-chat-filter",
            self.ti(&self.chat_filter),
        ));

        // ── история активного канала с фильтром ──
        // M14/F19: (id, строка) — id нужен для cmd/chat.delete.
        let messages: Vec<(Option<u64>, String)> = self
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
                    .map(|m| (m.id, format!("{}: {}", m.name, m.text)))
                    .collect()
            })
            .unwrap_or_default();
        let color = channel_color(&active, dark);
        // M11.4: stick-to-bottom — новая строка доскроливается вниз
        // (ScrollHandle + ScrollArea; egui-паритет ФР-13).
        // Фикс M14: история — один ребёнок ScrollArea, поэтому
        // прямой scroll_to_bottom (по индексу был бы no-op).
        if messages.len() != self.last_chat_len {
            self.last_chat_len = messages.len();
            if !messages.is_empty() {
                self.chat_scroll.scroll_to_bottom();
            }
        }
        // M14/F8: размер шрифта чата из настроек (10…20 px).
        let font = self.config.chat_font_px;
        // M14/F18: строки прижаты слева поверху (justify_start явно).
        let mut history = div().flex().flex_col().gap_1().justify_start();
        if messages.is_empty() {
            history = history.child(
                div()
                    .text_size(px(font))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::ChEmpty)),
            );
        }
        // M14/F19: строка = текст (+ ✕ удаления для админ/модераторов).
        let moderator = self.store.can_moderate();
        for (id, line) in messages {
            let text = div().text_size(px(font)).text_color(rgb(color)).child(line);
            let can_delete = moderator && id.is_some();
            history = history.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .child(div().flex_grow_1().min_w_0().child(text))
                    .when(can_delete, |row| {
                        let id = id.unwrap_or_default();
                        row.child(
                            IconButton::new(format!("ld2-chat-del-{id}"), IconName::X)
                                .tooltip(t(lang, Key::ChDelete))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_chat_msg(id);
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }
        // M14/F7: история занимает остаток блока журнала (но не меньше
        // прежних 120 px), чтобы ввод всегда оставался внизу. ScrollArea
        // — flex-элемент (а не дитя auto-высоты): иначе у Ely
        // `size_full()` схлопывается в auto и история льётся наружу.
        panel = panel.child(
            div()
                .flex()
                .flex_col()
                .flex_grow_1()
                .min_h(px(120.0))
                .child(
                    ScrollArea::new("ld2-chat-history")
                        .vertical()
                        .track_scroll(&self.chat_scroll)
                        .flex_grow_1()
                        .min_h_0()
                        .child(history),
                ),
        );

        // M14/F19: system-канал — ввод только для админ/модераторов
        // (сервер и так режет E_PERMISSION, здесь — UX без обмана).
        if active == "system" && !self.store.can_moderate() {
            panel = panel.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::SystemChatLocked)),
            );
            return panel;
        }

        // ── ввод: Enter/кнопка, слэш-команды (ФР-13) ──
        let mut input_row = div().flex().flex_row().gap_1().items_center();
        input_row = input_row.child(
            div()
                .flex_grow_1()
                .child(Input::new(self.ti(&self.chat_input))),
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
                        if let Some(input) = this.chat_input.clone() {
                            input.update(cx, |i, cx| i.set_text(text.clone(), cx));
                        }
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

    /// M14/F19: удаление сообщения из истории (`cmd/chat.delete`);
    /// права проверяет сервер → ошибки тостом (Act::ChatDelete).
    pub(crate) fn delete_chat_msg(&mut self, id: u64) {
        let cmd = ChatDelete { id };
        match self.transport.send(&cmd) {
            Ok(_) => self.store.on_sheet_sent(Act::ChatDelete),
            Err(e) => self.store.notice = Some(format!("нет связи: {e}")),
        }
    }

    /// Отправка ввода чата: слэш-команды парсятся, неизвестная — тост
    /// и в чат НЕ уходит (ФР-13).
    pub(crate) fn send_chat(&mut self, cx: &mut Context<'_, Self>) {
        let text = self.ti(&self.chat_input).read(cx).text().to_string();
        if text.trim().is_empty() {
            return;
        }
        if let Some(input) = self.chat_input.clone() {
            input.update(cx, |i, cx| i.set_text(String::new(), cx));
        }
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

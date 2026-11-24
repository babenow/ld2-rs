//! Тело чата gpui (M10.5, M14/F7 — вкладка «Чат» блока журнала) — порт
//! `ui/chat_panel.rs` egui-клиента:
//! вкладки шести каналов (§5), история с фильтром, ввод со слэш-командами
//! (ФР-13), «ответить» последнему адресату whisper.

use gpui::{div, prelude::*, px, rgb, Context, Div};
use ld2_core::l10n::{t, tf, Key};
use ld2_core::store::{parse_chat_input, Act, ChatDraft, CHAT_CHANNELS};
use ld2_protocol::cmd::{ChatDelete, ChatSend};

use crate::fa::fa_icon;
use crate::palette::{palette, Palette};
use ely::buttons::IconButton;
use ely::forms::{Choice, Input, SearchInput};
use ely::layout::ScrollArea;
use ely::navigation::Tabs;
use ely::primitives::IconName;

use super::{act_btn, Root, SendSlot};

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
        // Заказчик 2026-11-20: в канал группы/гильдии писать нельзя,
        // если не состоишь — метка 🔒 и отказ при переключении.
        let choices: Vec<Choice> = CHAT_CHANNELS
            .iter()
            .map(|channel| {
                let count = self.store.chat.get(*channel).map(|h| h.len()).unwrap_or(0);
                let locked = (*channel == "party" && self.store.party.is_none())
                    || (*channel == "guild" && self.store.guild.is_none());
                let lock = if locked { " 🔒" } else { "" };
                Choice::new(
                    *channel,
                    format!("{} ({count}){lock}", t(lang, channel_key(channel))),
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
                        let locked = match value.as_str() {
                            "party" => root.store.party.is_none(),
                            "guild" => root.store.guild.is_none(),
                            _ => false,
                        };
                        if locked {
                            root.store.notice = Some(if value == "party" {
                                t(root.lang, Key::ChNeedParty).to_string()
                            } else {
                                t(root.lang, Key::ChNeedGuild).to_string()
                            });
                            cx.notify();
                            return;
                        }
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
        // M14/F7: история занимает остаток блока журнала — flex_grow_1
        // и так прижимает ввод к низу. M14/F20: пол min_h(120) убран —
        // сумма минимумов панели (216px) была больше того, что журнал
        // даёт на своём поле min_h(180) (~140px), и ввод вылезал за
        // рамку чата; теперь панель сжимается до ~96px и влезает всегда.
        // ScrollArea остаётся flex-элементом (а не дитя auto-высоты):
        // иначе у Ely `size_full()` схлопывается в auto и история
        // льётся наружу.
        panel = panel.child(
            div().flex().flex_col().flex_grow_1().min_h_0().child(
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
        // M14/F20: min_w_0 на строке и обёртке — поле сжимается вместе
        // с колонкой и не вылезает за рамку (в т.ч. с кнопкой «Ответить»).
        // Заказчик 2026-11-20: кнопка «отправить» — иконкой, высота
        // равна высоте поля ввода (items_stretch); таймаут отправки.
        let cooldown_left = self
            .chat_cooldown_until
            .map(|t| t.saturating_duration_since(std::time::Instant::now()))
            .unwrap_or_default();
        let cooling = !cooldown_left.is_zero();
        const INPUT_H: f32 = 28.0; // высота поля Ely (ControlSize::Md)
        let mut input_row = div().flex().flex_row().gap_1().min_w_0();
        input_row = input_row.child(
            div()
                .flex_grow_1()
                .min_w_0()
                .h(px(INPUT_H))
                .child(Input::new(self.ti(&self.chat_input))),
        );
        if cooling {
            input_row = input_row.child(
                div()
                    .flex()
                    .items_center()
                    .text_size(px(11.0))
                    .text_color(rgb(pal.muted))
                    .child(format!("{:.0}с", cooldown_left.as_secs_f32().ceil())),
            );
        }
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
        // Кнопка «отправить» — иконка (Lucide Send); при кулдауне —
        // приглушённая. Высота равна полю ввода (items_stretch).
        let send_btn = div()
            .h(px(INPUT_H))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .bg(rgb(if cooling { pal.panel } else { pal.accent }))
            .text_color(rgb(if cooling { pal.muted } else { pal.accent_text }))
            .child(fa_icon(
                "solid/paper-plane",
                px(14.0),
                rgb(if cooling { pal.muted } else { pal.accent_text }),
            ))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| this.send_chat(cx)),
            );
        input_row = input_row.child(send_btn);
        panel = panel.child(input_row);
        panel
    }

    /// M14/F19: удаление сообщения из истории (`cmd/chat.delete`);
    /// права проверяет сервер → ошибки тостом (Act::ChatDelete).
    pub(crate) fn delete_chat_msg(&mut self, id: u64) {
        let cmd = ChatDelete { id };
        if self.send_or_notice(&cmd, SendSlot::Notice) {
            self.store.on_sheet_sent(Act::ChatDelete)
        }
    }

    /// Отправка ввода чата: слэш-команды парсятся, неизвестная — тост
    /// и в чат НЕ уходит (ФР-13).
    ///
    /// M18/F41: после отправки (и по Enter в пустом поле) фокус снимается
    /// — иначе гейт `typing_focused` глушит хоткеи окон. При ошибке
    /// отправки/неизвестной команде фокус остаётся: можно исправить.
    pub(crate) fn send_chat(&mut self, cx: &mut Context<'_, Self>) {
        // Таймаут отправки (анти-спам, заказчик): сервер своё окно
        // тоже проверяет; здесь — не отправляем и не очищаем поле.
        if self
            .chat_cooldown_until
            .is_some_and(|t| t > std::time::Instant::now())
        {
            return;
        }
        let typed = self.ti(&self.chat_input).read(cx).text().to_string();
        if typed.trim().is_empty() {
            Self::blur_typing(cx);
            return;
        }
        // Мат-фильтр (вкл. в настройках): маскируем до отправки и
        // показываем в поле отфильтрованный вариант.
        let text = if self.config.chat_mute_filter {
            let filtered = filter_profanity(&typed);
            if filtered != typed {
                if let Some(input) = self.chat_input.clone() {
                    let filtered = filtered.clone();
                    input.update(cx, |i, cx| i.set_text(filtered, cx));
                }
            }
            filtered
        } else {
            typed.clone()
        };
        let channel = self.chat_channel.clone();
        // Заказчик 2026-11-20: отправка в канал группы/гильдии — только
        // при membership (сервер и так отклонит; здесь — UX без обмана).
        if channel == "party" && self.store.party.is_none() {
            self.store.notice = Some(t(self.lang, Key::ChNeedParty).to_string());
            return;
        }
        if channel == "guild" && self.store.guild.is_none() {
            self.store.notice = Some(t(self.lang, Key::ChNeedGuild).to_string());
            return;
        }
        let reply = self.store.last_whisper_from.clone();
        match parse_chat_input(&text, &channel, reply.as_deref()) {
            ChatDraft::Send { channel, to, text } => {
                let cmd = ChatSend { channel, text, to };
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_sheet_sent(Act::Chat);
                    // Ревью R-мелочь: очищаем ПОСЛЕ успешной
                    // отправки — при Err набранный текст сохранён.
                    if let Some(input) = self.chat_input.clone() {
                        input.update(cx, |i, cx| i.set_text(String::new(), cx));
                    }
                    // История ввода (↑/↓ — заказчик).
                    self.chat_history.push(typed);
                    while self.chat_history.len() > 50 {
                        self.chat_history.remove(0);
                    }
                    self.chat_history_pos = None;
                    // Таймаут отправки: окно из §5 (chat.cooldown_s).
                    let cd = self
                        .store
                        .session
                        .as_ref()
                        .map(|s| s.chat.cooldown_s)
                        .unwrap_or(1.0);
                    self.chat_cooldown_until = Some(
                        std::time::Instant::now() + std::time::Duration::from_secs_f64(cd.max(0.0)),
                    );
                    Self::blur_typing(cx);
                }
            }
            ChatDraft::Unknown(cmd) => {
                self.store.notice =
                    Some(tf(self.lang, Key::ChUnknownCmd, &[("{cmd}", cmd.as_str())]));
            }
        }
    }

    /// Заказчик 2026-11-20: ↑/↓ в поле чата — листать историю ввода:
    /// ↑ — к более старым, ↓ — к более новым (ниже последней — пустое
    /// поле). Работает, пока фокус в поле чата (root.handle_keystroke).
    pub(crate) fn chat_history_nav(&mut self, older: bool, cx: &mut Context<'_, Self>) {
        if self.chat_history.is_empty() {
            return;
        }
        let new_pos = match self.chat_history_pos {
            None => {
                if !older {
                    return;
                }
                self.chat_history.len() - 1
            }
            Some(pos) => {
                if older {
                    if pos == 0 {
                        return;
                    }
                    pos - 1
                } else {
                    pos + 1
                }
            }
        };
        if new_pos >= self.chat_history.len() {
            self.chat_history_pos = None;
        } else {
            self.chat_history_pos = Some(new_pos);
        }
        let text = self
            .chat_history_pos
            .and_then(|pos| self.chat_history.get(pos))
            .cloned()
            .unwrap_or_default();
        if let Some(input) = self.chat_input.clone() {
            input.update(cx, |i, cx| i.set_text(text, cx));
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

// ── мат-фильтр (заказчик 2026-11-20; вкл/выкл в настройках) ──────────

/// Стемы нецензурных слов (нормализация: lowercase, ё→е). Стемы
/// отбирались так, чтобы не задевать обычные слова («небо», «сучок»,
/// «гондола», «хребет» — безопасны).
const PROFANITY_STEMS: &[&str] = &[
    // ru
    "хуй",
    "хуе",
    "хуя",
    "хуи",
    "наху",
    "поху",
    "охуе",
    "охуи",
    "ахуе",
    "ахуи",
    "заеб",
    "выеб",
    "уеб",
    "ъеб",
    "приеб",
    "перееб",
    "доеб",
    "отъеб",
    "ебал",
    "ебан",
    "ебат",
    "ебут",
    "ебищ",
    "хуеп",
    "хуес",
    "пизд",
    "спизд",
    "распиз",
    "запиз",
    "опиз",
    "бля",
    "сука",
    "суки",
    "суку",
    "суке",
    "мудак",
    "мудил",
    "гондон",
    "пидор",
    "пидар",
    "пидр",
    "шлюх",
    "мраз",
    // en
    "fuck",
    "fuk",
    "shit",
    "bitch",
    "cunt",
    "asshole",
    "bastard",
    "dickhead",
    "motherfuc",
];

/// Маскирует слово, если нормализованная форма содержит стем:
/// «*» вместо каждой буквы (длина сохраняется).
fn mask_word(word: &str) -> String {
    let norm: String = word.to_lowercase().replace('ё', "е");
    if PROFANITY_STEMS.iter().any(|stem| norm.contains(stem)) {
        "*".repeat(word.chars().count())
    } else {
        word.to_string()
    }
}

/// Мат-фильтр исходящего текста: слова из букв проверяются по стемам,
/// небуквенные символы (пробелы/знаки) копируются как есть.
pub(crate) fn filter_profanity(text: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    for ch in text.chars() {
        if ch.is_alphabetic() {
            word.push(ch);
        } else {
            if !word.is_empty() {
                out.push_str(&mask_word(&word));
                word.clear();
            }
            out.push(ch);
        }
    }
    if !word.is_empty() {
        out.push_str(&mask_word(&word));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_masks_by_stem_any_case() {
        assert_eq!(filter_profanity("привет ХУЙ"), "привет ***");
        assert_eq!(filter_profanity("Сука блин"), "**** блин");
        assert_eq!(filter_profanity("ПИЗДЕЦ"), "******");
        assert_eq!(filter_profanity("fuck you"), "**** you");
    }

    #[test]
    fn filter_normalizes_yo_and_keeps_innocent() {
        assert_eq!(filter_profanity("сушёная хурма"), "сушёная хурма");
        assert_eq!(
            filter_profanity("небо, сучок и гондола"),
            "небо, сучок и гондола"
        );
        assert_eq!(filter_profanity("хрEBет"), "хрEBет");
    }

    #[test]
    fn filter_keeps_punctuation_and_length() {
        assert_eq!(filter_profanity("привет, пидар!!"), "привет, *****!!");
        assert_eq!(filter_profanity(""), "");
    }
}

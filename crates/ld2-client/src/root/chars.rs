//! Экран персонажей gpui (M10.6, ФР-02; M11.7, D19–D21) — карточки с
//! цветом ника по классу, создание (имя + профиль), удаление: ввод ника
//! + таймер (вход в игру отменяет). Диалоги — Ely `Dialog` поверх.

use ld2_core::domain;

use ely::forms::{Choice, Input, Select};
use ely::layout::{Card, ScrollArea};
use ely::overlays::Dialog;
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div};
use ld2_core::l10n::{t, tf, Key, Lang};
use ld2_core::store::{local_epoch, Act, Gender, Profile};
use ld2_protocol::cmd::{CharCreate, CharSelect};
use ld2_protocol::types::CharacterSummary;

use crate::palette::{palette, Palette};

use super::{act_btn, Root, SendSlot};

const MIN_NAME_CHARS: usize = 2;
const DEATH_TIMER_CHOICES: [i64; 4] = [5, 10, 30, 60];
const DEATH_TIMER_DEFAULT_S: i64 = 10;

impl Root {
    /// M11.2: диалоги экрана персонажей (ФР-02): создание/удаление.
    pub(crate) fn chars_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if self.create_open {
            let title = t(self.lang, Key::CreateTitle).to_string();
            let body = self.create_view(cx, pal).into_any_element();
            let close = self.dismiss(cx, |r| {
                r.create_open = false;
                r.create_submitted = false;
                r.store.create_error = None;
            });
            return Some(
                Dialog::new("ld2-create", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        if self.delete_target.is_some() {
            let title = t(self.lang, Key::DeleteTitle).to_string();
            let body = self.delete_view(cx, pal).into_any_element();
            let close = self.dismiss(cx, |r| r.delete_target = None);
            return Some(
                Dialog::new("ld2-delete", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        None
    }

    pub(crate) fn characters_screen(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(session) = self.store.session.clone() else {
            // Ещё нет session.start (resume в процессе).
            return div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(t(lang, Key::Loading));
        };
        let (used, limit) = self.store.slot_usage();
        let pending = self.store.pending.is_some();

        let mut col = div()
            .flex()
            .flex_col()
            .gap_2()
            .items_center()
            .size_full()
            .p(px(crate::ui_tokens::SPACE_16))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(18.0))
                    .child(t(lang, Key::CharactersTitle)),
            )
            .child(div().text_color(rgb(pal.muted)).child(format!(
                "{} · {}",
                session.server.name,
                tf(
                    lang,
                    Key::SlotsUsed,
                    &[("used", &used.to_string()), ("limit", &limit.to_string())]
                )
            )));

        if let Some(err) = &self.store.select_error {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }
        if let Some(notice) = &self.store.notice {
            col = col.child(div().text_color(rgb(pal.muted)).child(notice.clone()));
        }

        // Список — отдельный скролл-лейаут: занимает всё оставшееся
        // место экрана и скроллится (запрос).
        let mut list = div().flex().flex_col().gap_2().items_center();
        // Аккаунт может быть без персонажей (M11.7) — подсказка.
        if session.characters.is_empty() {
            list = list.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::CharactersEmpty)),
            );
        }

        let dark = pal.bg == palette(domain::THEME_DARK).bg;
        for character in &session.characters {
            list = list.child(self.chars_card(cx, pal, character, dark));
        }
        col = col.child(
            ScrollArea::new("ld2-chars-list")
                .vertical()
                .w_full()
                .flex_grow_1()
                .min_h_0()
                .child(list),
        );

        // Кнопки действий — внизу окна (запрос): создание + выход.
        col = col.child(self.chars_toolbar(cx, pal, lang, used as u32 >= limit, pending));

        col
    }

    /// Карточка персонажа: ник цветом класса, метки преступлений,
    /// сводка `summary · location` и подвал (таймер удаления, вход,
    /// удаление). Вынесено из `characters_screen` (RF3t), тело как есть.
    fn chars_card(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        character: &CharacterSummary,
        dark: bool,
    ) -> Card {
        let lang = self.lang;
        let id = character.id.clone();
        let name = character.name.clone();
        let scheduled = self
            .store
            .pending_delete
            .as_ref()
            .filter(|p| p.char_id == character.id);

        // M11.7 (D19): ник цветом по классу; без profile — нейтрально.
        let mut head = div().flex().flex_row().gap_2().items_center().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(crate::ui_tokens::FONT_TITLE))
                .text_color(rgb(class_color(character.profile.as_deref(), dark)))
                .child(name.clone()),
        );
        for kind in &character.crime {
            // kind — игровые данные сервера (marauder/…) — как есть.
            head = head.child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_SM))
                    .text_color(rgb(pal.warn))
                    .child(kind.clone()),
            );
        }
        let meta = div()
            .text_size(px(crate::ui_tokens::FONT_SM))
            .text_color(rgb(pal.muted))
            .child(format!(
                "{} · {}",
                character.summary.clone().unwrap_or_default(),
                character.location
            ));

        // Подвал: отсчёт таймера (D20) или кнопки входа/удаления.
        let mut footer = div().flex().flex_row().gap_2().items_center();
        if let Some(p) = scheduled {
            let left = (p.deadline - local_epoch()).max(0);
            footer = footer.child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_MD))
                    .text_color(rgb(pal.danger))
                    .child(tf(lang, Key::DeleteIn, &[("s", &left.to_string())])),
            );
            footer = footer.child(act_btn(
                cx,
                t(lang, Key::DeleteCancelBtn),
                pal.panel,
                pal.text,
                |this, _cx| this.store.cancel_delete(),
            ));
        } else {
            let cid = id.clone();
            let cname = name.clone();
            footer = footer.child(act_btn(
                cx,
                t(lang, Key::BtnDelete),
                pal.panel,
                pal.danger,
                move |this, cx| {
                    // Одновременно — не больше одного отложенного (D20).
                    if this.store.pending.is_none() && this.store.pending_delete.is_none() {
                        this.delete_target = Some((cid.clone(), cname.clone()));
                        if let Some(input) = this.delete_name.clone() {
                            input.update(cx, |i, cx| i.set_text(String::new(), cx));
                        }
                    }
                },
            ));
            let cid2 = id.clone();
            footer = footer.child(act_btn(
                cx,
                t(lang, Key::BtnEnter),
                pal.accent,
                pal.accent_text,
                move |this, cx| {
                    if this.store.pending.is_none() {
                        // R3: min_client новее клиента — вход в мир
                        // заблокирован, команда не отправляется.
                        if this.store.forced_update {
                            this.store.select_error = Some(format!(
                                "⚠ Сервер требует клиент v{} (у вас v{}) — обновитесь",
                                this.store.min_client_required.clone().unwrap_or_default(),
                                env!("CARGO_PKG_VERSION")
                            ));
                            return;
                        }
                        let cmd = CharSelect {
                            char_id: cid2.clone(),
                        };
                        if this.send_or_notice(&cmd, SendSlot::Select) {
                            this.store.on_select_sent()
                        }
                    }
                    let _ = cx;
                },
            ));
        }
        Card::new()
            .w(px(420.0))
            .child(head)
            .child(meta)
            .footer(footer)
    }

    /// Подвал экрана персонажей: «Создать» (или «Слоты заняты» при
    /// лимите/занятости) и «Выйти». Вынесено из `characters_screen`
    /// (RF3t), тело перенесено как есть.
    fn chars_toolbar(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        lang: Lang,
        full: bool,
        pending: bool,
    ) -> Div {
        let create_label = if full {
            t(lang, Key::SlotsFull)
        } else {
            t(lang, Key::BtnCreate)
        };
        let mut toolbar = div()
            .flex()
            .flex_row()
            .gap_2()
            .mt(px(crate::ui_tokens::SPACE_4))
            .flex_shrink_0();
        if !full && !pending {
            toolbar = toolbar.child(act_btn(
                cx,
                create_label,
                pal.accent,
                pal.accent_text,
                |this, cx| {
                    this.create_open = true;
                    this.create_submitted = false;
                    if let Some(name) = this.create_name.clone() {
                        name.update(cx, |i, cx| i.set_text(String::new(), cx));
                    }
                },
            ));
        } else {
            toolbar = toolbar.child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.border))
                    .text_color(rgb(pal.muted))
                    .child(create_label),
            );
        }
        toolbar = toolbar.child(act_btn(
            cx,
            t(lang, Key::BtnLogout),
            pal.panel,
            pal.danger,
            |this, cx| {
                this.logout();
                let _ = cx;
            },
        ));
        toolbar
    }

    // ── создание персонажа (ФР-02) ────────────────────────────────────────

    fn create_view(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let pending = self.store.pending == Some(Act::Create);
        let name = self.ti(&self.create_name).read(cx).text().to_string();
        let name_valid = name.trim().chars().count() >= MIN_NAME_CHARS;
        let profile = self.create_profile;

        // Успех (ack без ошибки) — окно закрывается, notice на экране.
        if self.create_submitted
            && self.store.pending.is_none()
            && self.store.create_error.is_none()
        {
            self.create_open = false;
            self.create_submitted = false;
        }

        let mut col = div().flex().flex_col().gap_2().child(
            div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::ProfileLabel)),
        );

        // Профиль: ряд кнопок (вместо radio egui).
        let mut profiles = div().flex().flex_row().gap_1();
        for candidate in Profile::all() {
            let selected = profile == candidate;
            profiles = profiles.child(act_btn(
                cx,
                t(lang, candidate.label_key()),
                if selected { pal.accent } else { pal.panel },
                if selected { pal.accent_text } else { pal.text },
                move |this, _cx| {
                    this.create_profile = candidate;
                },
            ));
        }
        col = col.child(profiles);
        col = col.child(
            div()
                .text_size(px(crate::ui_tokens::FONT_SM))
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::ProfileHint)),
        );

        // M18/F34: пол — ряд кнопок под профилем; задаётся при создании
        // (диалоги NPC «знают» пол, F35).
        col = col.child(
            div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::GenderLabel)),
        );
        let gender = self.create_gender;
        let mut genders = div().flex().flex_row().gap_1();
        for candidate in Gender::all() {
            let selected = gender == candidate;
            genders = genders.child(act_btn(
                cx,
                t(lang, candidate.label_key()),
                if selected { pal.accent } else { pal.panel },
                if selected { pal.accent_text } else { pal.text },
                move |this, _cx| {
                    this.create_gender = candidate;
                },
            ));
        }
        col = col.child(genders);

        // Имя персонажа.
        col = col.child(
            div()
                .w(px(320.0))
                .child(Input::new(self.ti(&self.create_name))),
        );
        if !name.is_empty() && !name_valid {
            col = col.child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_SM))
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::NameTooShort)),
            );
        }
        if let Some(err) = &self.store.create_error {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }

        let mut row = div()
            .flex()
            .flex_row()
            .gap_2()
            .mt(px(crate::ui_tokens::SPACE_6));
        row = row.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, _cx| {
                this.create_open = false;
                this.create_submitted = false;
                this.store.create_error = None;
            },
        ));
        if name_valid && !pending {
            let cmd_name = name.trim().to_string();
            let cmd_profile = profile.value().to_string();
            row = row.child(act_btn(
                cx,
                t(lang, Key::BtnCreate),
                pal.accent,
                pal.accent_text,
                move |this, _cx| {
                    let cmd = CharCreate {
                        name: cmd_name.clone(),
                        profile: cmd_profile.clone(),
                        // M18/F34: пол из селектора формы.
                        gender: Some(this.create_gender.value().to_string()),
                    };
                    if this.send_or_notice(&cmd, SendSlot::Create) {
                        this.store.on_create_sent();
                        this.create_submitted = true;
                    }
                },
            ));
        } else {
            row = row.child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.border))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::BtnCreate)),
            );
        }
        col = col.child(row);
        col
    }

    // ── удаление персонажа (ФР-02, M11.7: ввод ника + таймер, D20/D21) ────

    fn delete_view(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some((id, name)) = self.delete_target.clone() else {
            return div().size_full();
        };
        let typed = self.ti(&self.delete_name).read(cx).text().to_string();
        let confirmed = typed == name;

        let mut col = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(tf(lang, Key::DeleteConfirm, &[("name", name.as_str())]))
            .child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_SM))
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::DeleteGameHint)),
            )
            .child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_SM))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::DeleteTypeNick)),
            )
            .child(
                div()
                    .w(px(320.0))
                    .child(Input::new(self.ti(&self.delete_name))),
            );
        if !typed.is_empty() && !confirmed {
            // Ник не совпадает — подсказка красным.
            col = col.child(
                div()
                    .text_size(px(crate::ui_tokens::FONT_SM))
                    .text_color(rgb(pal.danger))
                    .child(t(lang, Key::DeleteTypeNick)),
            );
        }

        // D21: длительность таймера — Select (5/10/30/60 с, по умолч. 10).
        let delay = self.delete_delay;
        let choices: Vec<Choice> = DEATH_TIMER_CHOICES
            .iter()
            .map(|s| Choice::new(s.to_string(), s.to_string()))
            .collect();
        let this = cx.entity();
        col = col.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::DeleteTimerLabel)),
                )
                .child(
                    Select::new("ld2-delete-timer", choices)
                        .selected(delay.to_string())
                        .on_change(move |v, _window, acx| {
                            let v: i64 = v.parse().unwrap_or(DEATH_TIMER_DEFAULT_S);
                            this.update(acx, |root, _cx| root.delete_delay = v);
                        }),
                ),
        );

        let mut row = div()
            .flex()
            .flex_row()
            .gap_3()
            .mt(px(crate::ui_tokens::SPACE_8));
        row = row.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, _cx| {
                this.delete_target = None;
            },
        ));
        if confirmed {
            let cid = id.clone();
            let cname = name.clone();
            row = row.child(act_btn(
                cx,
                t(lang, Key::BtnDelete),
                pal.danger,
                0xffffff,
                move |this, cx| {
                    // D20: не шлём сразу — ставим таймер; тикает он в tick.
                    let delay = this.delete_delay;
                    this.store
                        .schedule_delete(cid.clone(), cname.clone(), local_epoch(), delay);
                    if let Some(input) = this.delete_name.clone() {
                        input.update(cx, |i, cx| i.set_text(String::new(), cx));
                    }
                    this.delete_target = None;
                },
            ));
        } else {
            row = row.child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.border))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::BtnDelete)),
            );
        }
        col = col.child(row);
        col
    }
}

/// Цвет ника по классу (D19/ADR-005): warrior/archer/mage (M17/F30b:
/// «Вор» → «Лучник»); светлая тема — затемнение; без profile —
/// нейтральный.
fn class_color(profile: Option<&str>, dark: bool) -> u32 {
    if !dark {
        return match profile {
            Some(domain::PROFILE_WARRIOR) => 0xa1_34_39,
            Some(domain::PROFILE_ARCHER) => 0x4c_7a_2f,
            Some(domain::PROFILE_MAGE) => 0x2e_65_b8,
            _ => 0x5a_5e_66,
        };
    }
    match profile {
        Some(domain::PROFILE_WARRIOR) => 0xe0_6c_75,
        Some(domain::PROFILE_ARCHER) => 0x98_c3_79,
        Some(domain::PROFILE_MAGE) => 0x61_af_ef,
        _ => 0xab_b2_bf,
    }
}

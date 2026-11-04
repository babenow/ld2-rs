//! Экран персонажей gpui (M10.6, ФР-02; M11.7, D19–D21) — карточки с
//! цветом ника по классу, создание (имя + профиль), удаление: ввод ника
//! + таймер (вход в игру отменяет). Диалоги — Ely `Dialog` поверх.

use ely::forms::{Choice, Input, Select};
use ely::layout::Card;
use ely::overlays::Dialog;
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div};
use ld2_core::l10n::{t, tf, Key};
use ld2_core::store::{local_epoch, Act, Profile};
use ld2_protocol::cmd::{CharCreate, CharSelect};

use crate::palette::{palette, Palette};

use super::{act_btn, Root};

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
            .p(px(16.0))
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

        let dark = pal.bg == palette("dark").bg;
        for character in &session.characters {
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
                    .text_size(px(16.0))
                    .text_color(rgb(class_color(character.profile.as_deref(), dark)))
                    .child(name.clone()),
            );
            for kind in &character.crime {
                // kind — игровые данные сервера (marauder/…) — как есть.
                head = head.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.warn))
                        .child(kind.clone()),
                );
            }
            let meta = div()
                .text_size(px(12.0))
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
                        .text_size(px(13.0))
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
                            let cmd = CharSelect {
                                char_id: cid2.clone(),
                            };
                            match this.transport.send(&cmd) {
                                Ok(_) => this.store.on_select_sent(),
                                Err(e) => this.store.select_error = Some(format!("нет связи: {e}")),
                            }
                        }
                        let _ = cx;
                    },
                ));
            }

            let card = Card::new()
                .w(px(420.0))
                .child(head)
                .child(meta)
                .footer(footer);
            col = col.child(card);
        }

        // Создание (слоты заняты — недоступно, ФР-02/§5 лимит).
        let full = used as u32 >= limit;
        let create_label = if full {
            t(lang, Key::SlotsFull)
        } else {
            t(lang, Key::BtnCreate)
        };
        if !full && !pending {
            col = col.child(act_btn(
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
            col = col.child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.border))
                    .text_color(rgb(pal.muted))
                    .child(create_label),
            );
        }

        col = col.child(act_btn(
            cx,
            t(lang, Key::BtnLogout),
            pal.panel,
            pal.danger,
            |this, cx| {
                this.logout();
                let _ = cx;
            },
        ));
        col
    }

    // ── создание персонажа (ФР-02) ────────────────────────────────────────

    fn create_view(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let pending = self.store.pending == Some(Act::Create);
        let name = self.ti(&self.create_name).read(cx).text().to_string();
        let name_valid = name.trim().chars().count() >= 2;
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
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::ProfileHint)),
        );

        // Имя персонажа.
        col = col.child(
            div()
                .w(px(320.0))
                .child(Input::new(self.ti(&self.create_name))),
        );
        if !name.is_empty() && !name_valid {
            col = col.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::NameTooShort)),
            );
        }
        if let Some(err) = &self.store.create_error {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }

        let mut row = div().flex().flex_row().gap_2().mt(px(6.0));
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
                    };
                    match this.transport.send(&cmd) {
                        Ok(_) => {
                            this.store.on_create_sent();
                            this.create_submitted = true;
                        }
                        Err(e) => this.store.create_error = Some(format!("нет связи: {e}")),
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
                    .text_size(px(12.0))
                    .text_color(rgb(pal.warn))
                    .child(t(lang, Key::DeleteGameHint)),
            )
            .child(
                div()
                    .text_size(px(12.0))
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
                    .text_size(px(12.0))
                    .text_color(rgb(pal.danger))
                    .child(t(lang, Key::DeleteTypeNick)),
            );
        }

        // D21: длительность таймера — Select (5/10/30/60 с, по умолч. 10).
        let delay = self.delete_delay;
        let choices: Vec<Choice> = [5, 10, 30, 60]
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
                            let v: i64 = v.parse().unwrap_or(10);
                            this.update(acx, |root, _cx| root.delete_delay = v);
                        }),
                ),
        );

        let mut row = div().flex().flex_row().gap_3().mt(px(8.0));
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

/// Цвет ника по классу (D19/ADR-005): warrior/rogue/mage; светлая тема —
/// затемнение; без profile — нейтральный.
fn class_color(profile: Option<&str>, dark: bool) -> u32 {
    if !dark {
        return match profile {
            Some("warrior") => 0xa1_34_39,
            Some("rogue") => 0x4c_7a_2f,
            Some("mage") => 0x2e_65_b8,
            _ => 0x5a_5e_66,
        };
    }
    match profile {
        Some("warrior") => 0xe0_6c_75,
        Some("rogue") => 0x98_c3_79,
        Some("mage") => 0x61_af_ef,
        _ => 0xab_b2_bf,
    }
}

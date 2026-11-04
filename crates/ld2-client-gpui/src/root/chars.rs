//! Экран персонажей gpui (M10.6, ФР-02) — порт `ui/screens.rs`:
//! список слотов с входом/удалением, модалка создания (имя + профиль),
//! подтверждение удаления. Модалки заменяют контент (шапка остаётся).

use ely::overlays::Dialog;
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div};
use ld2_core::l10n::{t, tf, Key};
use ld2_core::store::{Act, Profile};
use ld2_protocol::cmd::{CharCreate, CharDelete, CharSelect};

use crate::palette::Palette;

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

        for character in &session.characters {
            let id = character.id.clone();
            let name = character.name.clone();
            let mut card = div()
                .flex()
                .flex_col()
                .gap_1()
                .w(px(420.0))
                .px_3()
                .py_2()
                .rounded_md()
                .bg(rgb(pal.panel))
                .border_1()
                .border_color(rgb(pal.border));
            let mut head = div().flex().flex_row().gap_2().items_center().child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(16.0))
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
            head = head.child(
                div()
                    .flex_grow_1()
                    .text_color(rgb(pal.muted))
                    .text_size(px(12.0))
                    .child(format!(
                        "{} · {}",
                        character.summary.clone().unwrap_or_default(),
                        character.location
                    )),
            );
            // Кнопки справа (как в egui right_to_left).
            let cid = id.clone();
            let cname = name.clone();
            head = head.child(act_btn(
                cx,
                t(lang, Key::BtnDelete),
                pal.panel,
                pal.danger,
                move |this, cx| {
                    if this.store.pending.is_none() {
                        this.delete_target = Some((cid.clone(), cname.clone()));
                    }
                    let _ = cx;
                },
            ));
            let cid2 = id.clone();
            head = head.child(act_btn(
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
            card = card.child(head);
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
                    this.create_name.update(cx, |i, _| i.content = "".into());
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
        let name = self.create_name.read(cx).content.to_string();
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
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(pal.border))
                .bg(rgb(pal.panel))
                .child(self.create_name.clone()),
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

    // ── удаление персонажа (ФР-02) ────────────────────────────────────────

    fn delete_view(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some((id, name)) = self.delete_target.clone() else {
            return div().size_full();
        };
        let pending = self.store.pending == Some(Act::Delete);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(tf(lang, Key::DeleteConfirm, &[("name", name.as_str())]))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .mt(px(8.0))
                    .child(act_btn(
                        cx,
                        t(lang, Key::Exit),
                        pal.panel,
                        pal.text,
                        |this, _cx| {
                            this.delete_target = None;
                        },
                    ))
                    .child({
                        let cid = id.clone();
                        act_btn(
                            cx,
                            t(lang, Key::BtnDelete),
                            pal.danger,
                            0xffffff,
                            move |this, _cx| {
                                if !pending {
                                    let cmd = CharDelete {
                                        char_id: cid.clone(),
                                    };
                                    match this.transport.send(&cmd) {
                                        Ok(_) => {
                                            this.store.on_delete_sent();
                                            this.delete_target = None;
                                        }
                                        Err(e) => {
                                            this.store.notice = Some(format!("нет связи: {e}"))
                                        }
                                    }
                                }
                            },
                        )
                    }),
            )
    }
}

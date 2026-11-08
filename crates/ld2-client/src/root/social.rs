//! Социальные окна gpui (M10.5): виджет группы, модалки приглашений
//! (ФР-14/16) и окно сделки (ФР-15) — порт `ui/world.rs` egui-клиента.
//!
//! Модалки заменяют контент игрового экрана (шапка остаётся) — паритет
//! egui-окон; версия/зеркало сделки — с сервера (клиент не пересчитывает).

use ely::overlays::Dialog;
use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Div};
use ld2_core::l10n::{t, tf, Key};
use ld2_core::store::Act;
use ld2_protocol::cmd::{
    PartyAccept, PartyCreate, PartyDecline, PartyKick, PartyLeave, TradeAccept, TradeAddItem,
    TradeCancel, TradePropose, TradeSetGold,
};

use crate::palette::Palette;
use ely::forms::{Choice, NumberInput, Select};

use ld2_protocol::types::PartyMember;

use super::{act_btn, Root};

/// M14/F2: окно группы показывается только когда участников ≥2 —
/// инвайт сам создаёт группу (сервер, F5), но до принятия приглашения
/// в ней один человек, и окно не должно занимать место.
pub(crate) fn show_party(members: &[PartyMember]) -> bool {
    members.len() >= 2
}

impl Root {
    /// M11.2: соц-диалоги (ФР-14/15/16): приглашения и сделка.
    /// Закрытие скримом = отклонение/отмена (сервер снимает состояние).
    pub(crate) fn social_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if let Some(invite) = self.store.party_invite.clone() {
            let title = t(self.lang, Key::PartyLabel).to_string();
            let body = self.party_invite_view(cx, pal, &invite).into_any_element();
            let id = invite.invite_id.clone();
            let close = self.dismiss(cx, move |r| {
                let cmd = PartyDecline {
                    invite_id: id.clone(),
                };
                let _ = r.transport.send(&cmd);
                r.store.party_invite = None;
            });
            return Some(
                Dialog::new("ld2-party-invite", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        if let Some(invite) = self.store.guild_invite.clone() {
            let title = t(self.lang, Key::GuildTab).to_string();
            let body = self.guild_invite_view(cx, pal, &invite).into_any_element();
            let close = self.dismiss(cx, |r| r.store.guild_invite = None);
            return Some(
                Dialog::new("ld2-guild-invite", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        if let Some(trade) = self.store.trade.clone() {
            let title = t(self.lang, Key::TradeLabel).to_string();
            let body = self.trade_view(cx, pal, &trade).into_any_element();
            let oid = trade.offer_id.clone();
            // Отмена — на сервере (закрыть окно = TradeCancel, как в egui).
            let close = self.dismiss(cx, move |r| {
                let cmd = TradeCancel {
                    offer_id: oid.clone(),
                };
                let _ = r.transport.send(&cmd);
            });
            return Some(
                Dialog::new("ld2-trade", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        None
    }

    // ── виджет группы (ФР-14) ─────────────────────────────────────────────

    pub(crate) fn party_widget(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let collapsed = self.party_collapsed;
        let party = self.store.party.clone();
        let my_id = self
            .store
            .snapshot
            .as_ref()
            .map(|s| s.char.id.clone())
            .unwrap_or_default();

        let mut block = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(pal.border));

        // Шапка: сворачивание + операции.
        let mut head = div().flex().flex_row().gap_1().items_center();
        head = head.child(act_btn(
            cx,
            t(lang, Key::PartyLabel),
            if collapsed { pal.panel } else { pal.accent },
            pal.text,
            |this, cx| {
                this.party_collapsed = !this.party_collapsed;
                this.config.party_collapsed = this.party_collapsed;
                this.save_config();
                let _ = cx;
            },
        ));
        if !collapsed {
            head = head.child(match &party {
                None => act_btn(
                    cx,
                    t(lang, Key::PartyCreateBtn),
                    pal.accent,
                    pal.accent_text,
                    |this, cx| {
                        let cmd = PartyCreate {};
                        match this.transport.send(&cmd) {
                            Ok(_) => this.store.on_sheet_sent(Act::Party),
                            Err(e) => this.store.notice = Some(format!("нет связи: {e}")),
                        }
                        let _ = cx;
                    },
                ),
                Some(_) => act_btn(
                    cx,
                    t(lang, Key::PartyLeaveBtn),
                    pal.panel,
                    pal.danger,
                    |this, cx| {
                        let cmd = PartyLeave {};
                        match this.transport.send(&cmd) {
                            Ok(_) => this.store.on_sheet_sent(Act::Party),
                            Err(e) => this.store.notice = Some(format!("нет связи: {e}")),
                        }
                        let _ = cx;
                    },
                ),
            });
        }
        block = block.child(head);

        if collapsed {
            return block;
        }
        let Some(party) = party else {
            block = block.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::PartyEmpty)),
            );
            return block;
        };

        for member in &party.members {
            let mut row = div().flex().flex_row().gap_2().items_center();
            let mut name_row = div().flex().flex_row().gap_1().items_center();
            if member.id == party.leader {
                name_row = name_row.child(div().text_color(rgb(pal.success)).child("★"));
            }
            name_row = name_row.child(div().child(member.name.clone()));
            if member.id == my_id {
                name_row = name_row.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(pal.muted))
                        .child("(вы)"),
                );
            }
            row = row.child(name_row.flex_grow_1());
            if let Some(hp) = &member.hp {
                let frac = hp.cur as f32 / hp.max.max(1) as f32;
                row = row.child(
                    div()
                        .w(px(60.0))
                        .h(px(8.0))
                        .rounded_md()
                        .bg(rgb(pal.border))
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative(frac.clamp(0.0, 1.0)))
                                .rounded_md()
                                .bg(rgb(pal.success)),
                        ),
                );
            }
            // Исключает только лидер и не себя (ФР-14).
            if party.leader == my_id && member.id != my_id {
                let pid = member.id.clone();
                row = row.child(act_btn(
                    cx,
                    t(lang, Key::PartyKickBtn),
                    pal.panel,
                    pal.danger,
                    move |this, cx| {
                        let cmd = PartyKick {
                            player_id: pid.clone(),
                        };
                        match this.transport.send(&cmd) {
                            Ok(_) => this.store.on_sheet_sent(Act::Party),
                            Err(e) => this.store.notice = Some(format!("нет связи: {e}")),
                        }
                        let _ = cx;
                    },
                ));
            }
            block = block.child(row);
        }
        block
    }

    // ── модалки приглашений (ФР-14/16) ────────────────────────────────────

    /// Приглашение в группу (§7 `party.accept/decline {invite_id}`).
    pub(crate) fn party_invite_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        invite: &ld2_core::store::PartyInviteView,
    ) -> Div {
        let lang = self.lang;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(tf(
                lang,
                Key::PartyInviteFrom,
                &[("{name}", invite.from_name.as_str())],
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .mt(px(8.0))
                    .child({
                        let id = invite.invite_id.clone();
                        act_btn(
                            cx,
                            t(lang, Key::PartyAcceptBtn),
                            pal.accent,
                            pal.accent_text,
                            move |this, cx| {
                                let cmd = PartyAccept {
                                    invite_id: id.clone(),
                                };
                                match this.transport.send(&cmd) {
                                    Ok(_) => this.store.on_sheet_sent(Act::Party),
                                    Err(e) => this.store.notice = Some(format!("нет связи: {e}")),
                                }
                                this.store.party_invite = None;
                                let _ = cx;
                            },
                        )
                    })
                    .child({
                        let id = invite.invite_id.clone();
                        act_btn(
                            cx,
                            t(lang, Key::PartyDeclineBtn),
                            pal.panel,
                            pal.text,
                            move |this, cx| {
                                let cmd = PartyDecline {
                                    invite_id: id.clone(),
                                };
                                match this.transport.send(&cmd) {
                                    Ok(_) => this.store.on_sheet_sent(Act::Party),
                                    Err(e) => this.store.notice = Some(format!("нет связи: {e}")),
                                }
                                this.store.party_invite = None;
                                let _ = cx;
                            },
                        )
                    }),
            )
    }

    /// Приглашение в гильдию (§7: принять = `guild.join`).
    pub(crate) fn guild_invite_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        invite: &ld2_core::store::GuildInviteView,
    ) -> Div {
        let lang = self.lang;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(tf(
                lang,
                Key::PartyInviteFrom,
                &[("{name}", invite.guild_name.as_str())],
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .mt(px(8.0))
                    .child({
                        let gid = invite.guild_id.clone();
                        act_btn(
                            cx,
                            t(lang, Key::GuildJoinBtn),
                            pal.accent,
                            pal.accent_text,
                            move |this, cx| {
                                let cmd = ld2_protocol::cmd::GuildJoin {
                                    guild_id: Some(gid.clone()),
                                };
                                match this.transport.send(&cmd) {
                                    Ok(_) => this.store.on_sheet_sent(Act::Guild),
                                    Err(e) => this.store.notice = Some(format!("нет связи: {e}")),
                                }
                                this.store.guild_invite = None;
                                let _ = cx;
                            },
                        )
                    })
                    .child(act_btn(
                        cx,
                        t(lang, Key::PartyDeclineBtn),
                        pal.panel,
                        pal.text,
                        |this, cx| {
                            this.store.guild_invite = None;
                            let _ = cx;
                        },
                    )),
            )
    }

    // ── окно сделки (ФР-15) ───────────────────────────────────────────────

    pub(crate) fn trade_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        trade: &ld2_protocol::evt::TradeState,
    ) -> Div {
        let lang = self.lang;
        let offer_id = trade.offer_id.clone();
        let version = trade.version;
        let inventory = self
            .store
            .snapshot
            .as_ref()
            .map(|s| s.char.inventory.clone())
            .unwrap_or_default();
        let offered: Vec<String> = trade.you.iter().map(|i| i.uid.clone()).collect();

        let mut col = div().flex().flex_col().gap_2();
        // Оценка + предупреждение (ФР-15 анти-абуз).
        if let Some(value) = trade.value_hint {
            col = col.child(div().text_color(rgb(pal.muted)).child(tf(
                lang,
                Key::TradeValue,
                &[("{v}", &value.to_string())],
            )));
        }
        col = col.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.warn))
                .child(t(lang, Key::TradeWarn)),
        );

        // Две колонки: я / партнёр (зеркало с сервера).
        col = col.child(
            div()
                .flex()
                .flex_row()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .w(px(240.0))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(pal.panel))
                        .border_1()
                        .border_color(rgb(pal.accent))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(t(lang, Key::TradeYou)),
                        )
                        .children(trade.you.iter().map(|item| {
                            div().child(format!("• {} ×{}", item.name, item.qty.unwrap_or(1)))
                        }))
                        .child(div().text_color(rgb(pal.muted)).child(format!(
                            "{}: {}",
                            t(lang, Key::TradeGold),
                            trade.you_gold
                        )))
                        .children((trade.you_locked).then(|| {
                            div()
                                .text_color(rgb(pal.success))
                                .child(format!("✓ {}", t(lang, Key::TradeAccept)))
                        })),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .w(px(240.0))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(pal.panel))
                        .border_1()
                        .border_color(rgb(pal.border))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(t(lang, Key::TradeThem)),
                        )
                        .children(trade.them.iter().map(|item| {
                            div().child(format!("• {} ×{}", item.name, item.qty.unwrap_or(1)))
                        }))
                        .child(div().text_color(rgb(pal.muted)).child(format!(
                            "{}: {}",
                            t(lang, Key::TradeGold),
                            trade.them_gold
                        )))
                        .children((trade.them_locked).then(|| {
                            div()
                                .text_color(rgb(pal.success))
                                .child(format!("✓ {}", t(lang, Key::TradeAccept)))
                        })),
                ),
        );

        // M11.3: добавление предмета — Select Ely + «+ предмет»
        // (отклонение «ряды кнопок вместо combobox» снят).
        let available: Vec<_> = inventory
            .iter()
            .filter(|i| !offered.contains(&i.uid))
            .collect();
        if !available.is_empty() {
            let choices: Vec<Choice> = available
                .iter()
                .map(|item| Choice::new(item.uid.clone(), item.name.clone()))
                .collect();
            let pick = self.trade_pick.clone();
            let this = cx.entity();
            let mut add_row = div().flex().flex_row().gap_2().items_center();
            add_row = add_row.child(div().flex_grow_1().child({
                let this = this.clone();
                let sel =
                    Select::new("ld2-trade-item", choices).placeholder(t(lang, Key::TradeAddItem));
                match pick {
                    Some(value) => sel.selected(value),
                    None => sel,
                }
                .on_change(move |value, _window, acx| {
                    let value = value.to_string();
                    this.update(acx, |root, _cx| root.trade_pick = Some(value));
                })
            }));
            if let Some(uid) = self.trade_pick.clone() {
                let oid = offer_id.clone();
                add_row = add_row.child(act_btn(
                    cx,
                    t(lang, Key::TradeAddItem),
                    pal.accent,
                    pal.accent_text,
                    move |this, cx| {
                        let cmd = TradeAddItem {
                            offer_id: oid.clone(),
                            item_uid: uid.clone(),
                            qty: None,
                        };
                        match this.transport.send(&cmd) {
                            Ok(_) => this.store.on_sheet_sent(Act::Trade),
                            Err(e) => this.store.notice = Some(format!("нет связи: {e}")),
                        }
                        let _ = cx;
                    },
                ));
            }
            col = col.child(add_row);
        }

        // Золото.
        let mut gold_row = div().flex().flex_row().gap_1().items_center();
        gold_row = gold_row.child(
            div()
                .text_color(rgb(pal.muted))
                .child(format!("{}:", t(lang, Key::TradeGold))),
        );
        gold_row = gold_row.child(div().w(px(140.0)).child({
            let this = cx.entity();
            let gold = self.trade_gold_value;
            NumberInput::new("ld2-trade-gold", gold)
                .precision(0)
                .step(10.0)
                .on_change(move |v, _window, acx| {
                    this.update(acx, |root, _cx| root.trade_gold_value = v);
                })
        }));
        {
            let oid = offer_id.clone();
            gold_row = gold_row.child(act_btn(
                cx,
                t(lang, Key::TradeSetGold),
                pal.panel,
                pal.text,
                move |this, _cx| {
                    let gold = this.trade_gold_value.max(0.0) as i64;
                    let cmd = TradeSetGold {
                        offer_id: oid.clone(),
                        gold,
                    };
                    match this.transport.send(&cmd) {
                        Ok(_) => this.store.on_sheet_sent(Act::Trade),
                        Err(e) => this.store.notice = Some(format!("нет связи: {e}")),
                    }
                },
            ));
        }
        col = col.child(gold_row);

        // Операции: фиксация/принятие/отмена (сверка версии — сервер, §9).
        let mut ops = div().flex().flex_row().gap_2();
        {
            let oid = offer_id.clone();
            ops = ops.child(act_btn(
                cx,
                t(lang, Key::TradePropose),
                pal.accent,
                pal.accent_text,
                move |this, cx| {
                    let cmd = TradePropose {
                        offer_id: oid.clone(),
                        version,
                    };
                    match this.transport.send(&cmd) {
                        Ok(_) => this.store.on_sheet_sent(Act::Trade),
                        Err(e) => this.store.notice = Some(format!("нет связи: {e}")),
                    }
                    let _ = cx;
                },
            ));
        }
        {
            let oid = offer_id.clone();
            ops = ops.child(act_btn(
                cx,
                t(lang, Key::TradeAccept),
                pal.success,
                0xffffff,
                move |this, cx| {
                    let cmd = TradeAccept {
                        offer_id: oid.clone(),
                        version,
                    };
                    match this.transport.send(&cmd) {
                        Ok(_) => this.store.on_sheet_sent(Act::Trade),
                        Err(e) => this.store.notice = Some(format!("нет связи: {e}")),
                    }
                    let _ = cx;
                },
            ));
        }
        {
            let oid = offer_id.clone();
            ops = ops.child(act_btn(
                cx,
                t(lang, Key::TradeCancel),
                pal.panel,
                pal.danger,
                move |this, cx| {
                    let cmd = TradeCancel {
                        offer_id: oid.clone(),
                    };
                    match this.transport.send(&cmd) {
                        Ok(_) => this.store.on_sheet_sent(Act::Trade),
                        Err(e) => this.store.notice = Some(format!("нет связи: {e}")),
                    }
                    let _ = cx;
                },
            ));
        }
        col = col.child(ops);
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str) -> PartyMember {
        PartyMember {
            id: format!("id_{name}"),
            name: name.to_string(),
            hp: None,
            location_id: None,
            ready: None,
            extra: Default::default(),
        }
    }

    /// M14/F2: пустая группа и «один в группе» — окно скрыто; с двумя
    /// и более — показано.
    #[test]
    fn party_widget_shown_only_from_two_members() {
        assert!(!show_party(&[]), "нет группы");
        assert!(!show_party(&[member("alice")]), "группа из одного");
        assert!(show_party(&[member("alice"), member("bob")]), "двое");
        assert!(
            show_party(&[member("a"), member("b"), member("c")]),
            "трое и больше"
        );
    }
}

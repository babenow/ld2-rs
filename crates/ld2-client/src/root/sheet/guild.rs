//! Окно персонажа: вкладка «Гильдия» (RF3l).

use super::*;

impl Root {
    pub(crate) fn tab_guild(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let target = self.store.target.clone();
        let guild = self.store.guild.clone();
        let my_id = self
            .store
            .snapshot
            .as_ref()
            .map(|s| s.char.id.clone())
            .unwrap_or_default();

        let Some(g) = guild else {
            return self.guild_join_block(lang, cx, pal);
        };
        self.guild_main_block(lang, target, my_id, g, cx, pal)
    }

    /// «Не в гильдии»: создать или вступить по списку мира (RF3l).
    fn guild_join_block(
        &mut self,
        lang: ld2_core::l10n::Lang,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Div {
        // Не в гильдии: создать или вступить по списку мира (§7).
        let mut col = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::GuildNone)),
            )
            .child(
                div()
                    .w(px(320.0))
                    .child(Input::new(self.ti(&self.guild_input))),
            )
            .child(act_btn(
                cx,
                t(lang, Key::GuildCreateBtn),
                pal.accent,
                pal.accent_text,
                |this, cx| {
                    let name = this.ti(&this.guild_input).read(cx).text().to_string();
                    let name = name.trim().to_string();
                    if !name.is_empty() {
                        let cmd = ld2_protocol::cmd::GuildCreate { name };
                        this.send_cmd(&cmd, Act::Guild);
                    }
                },
            ));
        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::GuildBrowseLbl)),
        );
        if self.store.guild_browse.is_empty() {
            col = col.child(div().text_color(rgb(pal.muted)).child("…"));
        }
        for entry in self.store.guild_browse.clone() {
            let id = entry["id"].as_str().unwrap_or("").to_string();
            let name = entry["name"].as_str().unwrap_or("?").to_string();
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .child(div().child(name))
                    .child(act_btn(
                        cx,
                        t(lang, Key::GuildJoinBtn),
                        pal.accent,
                        pal.accent_text,
                        move |this, cx| {
                            let cmd = ld2_protocol::cmd::GuildJoin {
                                guild_id: Some(id.clone()),
                            };
                            this.send_cmd(&cmd, Act::Guild);
                            let _ = cx;
                        },
                    )),
            );
        }
        col
    }

    /// В гильдии: инфо, состав, операции, журнал (RF3l).
    fn guild_main_block(
        &mut self,
        lang: ld2_core::l10n::Lang,
        target: Option<String>,
        my_id: String,
        g: ld2_protocol::types::GuildState,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Div {
        // В гильдии: инфо, состав, операции, журнал.
        let mut col = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(g.name.clone()),
            )
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(crate::ui_tokens::FONT_MD))
                    .child(t(lang, Key::GuildMembers)),
            );
        let i_am_master = g
            .members
            .iter()
            .any(|m| m.id == my_id && m.rank == "master");
        for member in &g.members {
            let mut row = div()
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .px_2()
                .rounded_md()
                .bg(rgb(pal.panel))
                .child(div().flex_grow_1().child(member.name.clone()))
                .child(
                    div()
                        .text_size(px(crate::ui_tokens::FONT_SM))
                        .text_color(rgb(if member.rank == "master" {
                            pal.warn
                        } else {
                            pal.muted
                        }))
                        .child(member.rank.clone()),
                );
            if member.online {
                row = row.child(
                    div()
                        .text_size(px(crate::ui_tokens::FONT_XS))
                        .text_color(rgb(pal.success))
                        .child("●"),
                );
            }
            if i_am_master && member.id != my_id {
                let pid = member.id.clone();
                let next = match member.rank.as_str() {
                    "master" => "officer",
                    "officer" => "member",
                    _ => "master",
                }
                .to_string();
                let pid2 = pid.clone();
                let rank = next.clone();
                row = row.child(act_btn(
                    cx,
                    format!("{} → {next}", t(lang, Key::GuildRankLbl)),
                    pal.panel,
                    pal.text,
                    move |this, cx| {
                        let cmd = GuildRankSet {
                            player_id: pid2.clone(),
                            rank: rank.clone(),
                        };
                        this.send_cmd(&cmd, Act::Guild);
                        let _ = cx;
                    },
                ));
                row = row.child(act_btn(
                    cx,
                    t(lang, Key::GuildKickBtn),
                    pal.panel,
                    pal.danger,
                    move |this, cx| {
                        let cmd = ld2_protocol::cmd::GuildKick {
                            player_id: pid.clone(),
                        };
                        this.send_cmd(&cmd, Act::Guild);
                        let _ = cx;
                    },
                ));
            }
            col = col.child(row);
        }

        let mut ops = div().flex().flex_row().gap_1();
        if let Some(target) = &target {
            let tid = target.clone();
            ops = ops.child(act_btn(
                cx,
                t(lang, Key::GuildInviteBtn),
                pal.panel,
                pal.text,
                move |this, cx| {
                    let cmd = ld2_protocol::cmd::GuildInvite {
                        player_id: tid.clone(),
                    };
                    this.send_cmd(&cmd, Act::Guild);
                    let _ = cx;
                },
            ));
        }
        let gid = g.id.clone();
        ops = ops.child(act_btn(
            cx,
            t(lang, Key::GuildLeaveBtn),
            pal.panel,
            pal.danger,
            move |this, cx| {
                let cmd = GuildLeave {
                    guild_id: Some(gid.clone()),
                };
                this.send_cmd(&cmd, Act::Guild);
                let _ = cx;
            },
        ));
        col = col.child(ops);

        if !self.store.guild_log.is_empty() {
            col = col.child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(crate::ui_tokens::FONT_MD))
                    .child(t(lang, Key::GuildLogLbl)),
            );
            for entry in self.store.guild_log.iter().rev().take(5) {
                col = col.child(
                    div()
                        .text_size(px(crate::ui_tokens::FONT_SM))
                        .text_color(rgb(pal.muted))
                        .child(format!("• {}", entry.text)),
                );
            }
        }
        col
    }
}

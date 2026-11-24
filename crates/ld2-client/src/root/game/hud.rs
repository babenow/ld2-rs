//! HUD игрового экрана: статус-бар, левая колонка, цель/локация,
//! история, журнал (RF3m).

use super::*;

impl Root {
    /// Статус-бар игрового экрана: слева — кнопка «Чат» (когда чат
    /// свёрнут), справа — бейдж статуса сервера (+ RTT).
    pub(crate) fn status_bar(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(rgb(pal.border))
            .flex_shrink_0();
        if self.config.chat_collapsed {
            bar = bar.child(
                div()
                    .id("ld2-statusbar-chat")
                    .px_1()
                    .cursor_pointer()
                    .tooltip(Tooltip::text(t(lang, Key::ChLabel)))
                    .child(fa_icon("regular/comment-dots", px(14.0), rgb(pal.text)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        // Разворачивание из статус-бара — высота 50 %.
                        this.config.chat_collapsed = false;
                        this.config.chat_height_pct = 0.5;
                        this.save_config();
                        cx.notify();
                    })),
            );
        }
        bar = bar.child(div().flex_grow_1());
        // Автор и версия клиента — в центре бара (заказчик).
        bar = bar.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(tf(
                    lang,
                    Key::StatusBarAuthor,
                    &[
                        ("{author}", crate::root::AUTHOR),
                        ("{v}", env!("CARGO_PKG_VERSION")),
                    ],
                )),
        );
        bar = bar.child(div().flex_grow_1());
        // M28/B8: сумки и ёмкость — справа, левее значка связи
        // (заказчик: иконки слотов под сумки, наведение — название и
        // занятость; клик открывает окно рюкзака).
        bar = bar.child(self.status_bags(cx, pal));
        if let Some((used, total)) = self.inventory_usage() {
            let full = used as u32 >= total;
            bar = bar.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(if full { pal.danger } else { pal.muted }))
                    .child(tf(
                        lang,
                        Key::BackpackUsage,
                        &[
                            ("{used}", &used.to_string()),
                            ("{total}", &total.to_string()),
                        ],
                    )),
            );
        }
        let status = self.store.transport.clone();
        bar = bar.child(status_badge(pal, lang, &status, self.store.rtt_ms));
        bar
    }

    // ── левая колонка: цель, ресурсы, история ─────────────────────────────

    pub(crate) fn left_column(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        snap: &StateSnapshot,
    ) -> Div {
        let lang = self.lang;
        // M24: ширина задаётся контейнером в game_screen (30 %,
        // резиновая); колонка тянется на всю его ширину.
        let mut col = div().flex().flex_col().gap_2().w_full();

        // Имя + бейджи статусов (ФР-11): серверные сроки, now = лок + offset.
        let mut name_row = div().flex().flex_row().items_center().gap_2().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(16.0))
                .child(snap.char.name.clone()),
        );
        let now = local_epoch() + self.store.time_offset;
        for status in filter_active_statuses(&snap.char.crime.statuses, now) {
            let title = crime_kind_title(&self.store, &status.kind);
            let left = status.expires_at - now;
            name_row = name_row.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(severity_rgb(status.severity)))
                    .child(format!("{title} · {}", format_duration(lang, left))),
            );
        }
        col = col.child(name_row);

        // Ресурсы (ФР-04/09).
        // M25: шкала ХП — виджет stat_bar (выше/шире — параметры,
        // цифры поверх бара, зелёная заливка).
        col = col.child(stat_bar(
            pal.success,
            snap.char.hp.max,
            snap.char.hp.cur,
            t(lang, Key::HpLabel),
            BAR_WIDTH,
            16.0,
            pal,
        ));
        // M18: ресурс по профилю — адреналин (красный), энергия
        // (тёмно-жёлтый), мана (акцентный); длина бара — общая
        // константа со шкалой здоровья.
        let (resource_label, resource_color) = match snap.char.profile.as_deref() {
            Some("warrior") => (t(lang, Key::RageLabel), 0xd1_3b_3b),
            Some("archer") => (t(lang, Key::EnergyLabel), 0xb8_96_1e),
            _ => (t(lang, Key::MpLabel), pal.accent),
        };
        // M25: шкала ресурса — тот же виджет (цвет по профилю).
        col = col.child(stat_bar(
            resource_color,
            snap.char.mp.max,
            snap.char.mp.cur,
            resource_label,
            BAR_WIDTH,
            16.0,
            pal,
        ));
        // M24: индикаторы вставленных рун (указание заказчика) —
        // под индикатором ресурса: глиф · прогрессбар заполнения ·
        // N/M (правее глифа бар, правее бара — заряды).
        for row in self.rune_indicator_rows(snap, pal) {
            col = col.child(row);
        }
        // M24: индикаторы бафов рун (указание заказчика): активные
        // баффы рун; за3 с до конца — моргание раз в секунду.
        for row in self.rune_buff_rows(snap, pal) {
            col = col.child(row);
        }
        col = col.child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(format!(
                    "{}: {}",
                    t(lang, Key::XpPoolLabel),
                    snap.char.xp_pool
                )),
        );
        // M24: единый прогресс-бар (разделка/инкрустация/заточка) —
        // под панелью персонажа (ХП/ресурс; плавность — быстрый тик).
        col = col.when_some(self.progress_bar(pal), |d, b| d.child(b));

        // M14/F1: «Сменить персонажа»/«Выйти» переехали в меню шапки.

        // M14/F2: окно группы — только при участниках ≥2 (инвайт сам
        // создаёт группу — F5, но до принятия она пустая).
        if self
            .store
            .party
            .as_ref()
            .is_some_and(|p| show_party(&p.members))
        {
            col = col.child(self.party_widget(cx, pal));
        }

        // M14/F7: чат переехал в блок журнала (вкладка «Чат»);
        // история посещений — M25 в правой колонке.
        col
    }

    /// Блок таргета (M14/F2: перенесён из левой колонки — под описание
    /// локации): имя, HP и кнопки «Атака»/«Побег».
    pub(crate) fn target_block(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        snap: &StateSnapshot,
    ) -> Div {
        let lang = self.lang;
        let Some(target_id) = self.store.target.clone() else {
            return div();
        };
        let Some(entity) = snap.location.entities.iter().find(|e| e.id == target_id) else {
            return div();
        };
        let name = entity.name.clone();
        let id = entity.id.clone();
        let hp = entity.hp;
        let mut block = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .border_1()
            .border_color(rgb(pal.border))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(format!("{}: {name}", t(lang, Key::TargetLabel))),
            );
        if let Some(hp) = hp {
            block = block.child(bar(
                hp.cur as f32 / hp.max.max(1) as f32,
                format!("{}/{}", hp.cur, hp.max),
                pal.danger,
                pal,
            ));
        }
        block = block.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(btn(
                    cx,
                    t(lang, Key::Attack).to_string(),
                    pal.danger,
                    0xffffff,
                    Click::Attack(id.clone()),
                ))
                .child(btn(
                    cx,
                    t(lang, Key::BtnFlee).to_string(),
                    pal.panel,
                    pal.text,
                    Click::Flee,
                )),
        );
        block
    }

    // ── центр: локация, выходы, сущности ──────────────────────────────────

    pub(crate) fn location_block(&mut self, pal: &Palette, loc: &LocationSnapshot) -> Div {
        // M25: без названия и флагов (заказчик): описание, дивидер
        // Ely и сразу лог — приклеен к низу описания.
        let mut col = div().flex().flex_col();
        col = col.child(
            div()
                .id("location_text")
                // M25 (заказчик): журнал ровно под текстом описания —
                // бокс по содержимому (без пустоты фиксированных 110px);
                // сверх max_h — прокрутка.
                .max_h(px(200.0))
                .overflow_y_scroll()
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(rgb(pal.text))
                        .child(loc.text.clone()),
                ),
        );
        col = col.child(Divider::horizontal());
        col.child(self.location_log(pal))
    }

    /// Тосты мира (world_error/notice) — как в egui-панели локации.
    pub(crate) fn notice_line(&self, pal: &Palette) -> Option<Div> {
        if let Some(err) = &self.store.world_error {
            return Some(div().text_color(rgb(pal.danger)).child(err.clone()));
        }
        if let Some(notice) = &self.store.notice {
            return Some(div().text_color(rgb(pal.warn)).child(notice.clone()));
        }
        None
    }

    pub(crate) fn exits_row(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        loc: &LocationSnapshot,
    ) -> Div {
        let lang = self.lang;
        // M25: вид по образцу заказчика — заголовок, разделитель
        // «---» и вертикальный список; стрелка — направление выхода
        // ИЗ локации (`dir`).
        let mut col = div().flex().flex_col().gap_1().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(format!("{}:", t(lang, Key::ExitsLabel))),
        );
        col = col.child(Divider::horizontal());
        if loc.exits.is_empty() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoExits)),
            );
        }
        for exit in &loc.exits {
            let id = exit.id.clone();
            let arrow = match exit.dir.as_deref() {
                Some("north") => "↑",
                Some("south") => "↓",
                Some("east") => "→",
                Some("west") => "←",
                _ => "",
            };
            let label = if arrow.is_empty() {
                exit.label.clone()
            } else {
                format!("{} {arrow}", exit.label)
            };
            col = col.child(btn(cx, label, pal.panel, pal.accent, Click::MoveExit(id)));
        }
        col
    }

    pub(crate) fn entities_block(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        loc: &LocationSnapshot,
    ) -> Div {
        let safe = loc.flags.iter().any(|f| f == "safe");
        let mut block = div().flex().flex_col().gap_1();
        for (title, kind) in [
            (Key::PlayersLabel, EntityKind::Player),
            (Key::MonstersLabel, EntityKind::Monster),
            (Key::NpcsLabel, EntityKind::Npc),
            (Key::CorpsesLabel, EntityKind::Corpse),
            // M20/F20b: объекты-ресурсы и предметы на земле.
            (Key::ResourcesLabel, EntityKind::Resource),
            (Key::ObjectsLabel, EntityKind::Object),
            // M20/F20c: контейнеры (сундуки) — лут осмотром.
            (Key::ContainersLabel, EntityKind::Container),
        ] {
            let group: Vec<Entity> = loc
                .entities
                .iter()
                .filter(|e| e.kind == kind)
                .cloned()
                .collect();
            if group.is_empty() {
                continue;
            }
            block = block.child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_size(px(13.0))
                    .child(t(self.lang, title)),
            );
            for entity in group {
                block = block.child(self.entity_row(cx, pal, entity, safe));
            }
        }
        block
    }

    // ── журнал мира (ФР-17) ───────────────────────────────────────────────

    /// M25: лог локации — внутри карточки описания (указание
    /// заказчика); автопрокрутка вниз (M14/F13).
    pub(crate) fn location_log(&mut self, pal: &Palette) -> Div {
        let len = self.store.log.len();
        // M14/F13: лог всегда промотан вниз — новая строка возвращает
        // вниз; вручную поднять можно до прихода следующей строки.
        if len != self.journal_seen && len > 0 {
            self.journal_scroll.scroll_to_bottom();
        }
        self.journal_seen = len;
        let dark = pal.bg == crate::palette::palette("dark").bg;
        // M14/F8: размер шрифта лога из настроек (10…20 px).
        let log_font = self.config.log_font_px;
        // M14/F13: цвета/жирность строк боя — из настроек.
        let damage_color = self.config.log_damage_color.clone();
        let attack_color = self.config.log_attack_color.clone();
        let damage_bold = self.config.log_damage_bold;
        div().flex().flex_col().min_h_0().h(px(96.0)).child(
            ScrollArea::new("ld2-log-scroll")
                .vertical()
                .track_scroll(&self.journal_scroll)
                .flex_grow_1()
                .min_h_0()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(self.store.log.iter().map(|entry| {
                            let (color, bold) = match entry.kind {
                                LogKind::CombatIn => {
                                    (hex_to_u32(&damage_color).unwrap_or(0xe0_6c_75), damage_bold)
                                }
                                LogKind::CombatOut => {
                                    (hex_to_u32(&attack_color).unwrap_or(0x61_af_ef), false)
                                }
                                kind => (log_color(kind, dark), false),
                            };
                            // M17/F30f: **...** — жирные сегменты
                            // (слова заклинания и урон), §8.
                            let text = entry.text.clone();
                            let line = div()
                                .text_size(px(log_font))
                                .text_color(rgb(color))
                                .when(bold, |line| line.font_weight(gpui::FontWeight::BOLD));
                            if text.contains("**") {
                                line.flex()
                                    .flex_row()
                                    .children(split_bold(&text).into_iter().map(
                                        |(seg, is_bold)| {
                                            div()
                                                .when(is_bold, |d| {
                                                    d.font_weight(gpui::FontWeight::BOLD)
                                                })
                                                .child(seg.to_string())
                                        },
                                    ))
                            } else {
                                line.child(text)
                            }
                        })),
                ),
        )
    }

    /// M25: чат — внизу центра (лог переехал в карточку локации);
    /// высота фиксированная —20 % игрового поля (заказчик).
    pub(crate) fn journal_block(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        // Шапка: заголовок + «свернуть» (заказчик 2026-11-20): чат
        // уходит целиком, открыть — кнопкой в статус-баре (высота 50 %).
        let head = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(t(lang, Key::ChLabel)),
            )
            .child(
                div()
                    .id("ld2-chat-collapse")
                    .px_1()
                    .cursor_pointer()
                    .tooltip(Tooltip::text(t(lang, Key::ChCollapse)))
                    .child(fa_icon("solid/chevron-down", px(12.0), rgb(pal.muted)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.config.chat_collapsed = true;
                        this.save_config();
                        cx.notify();
                    })),
            );
        // Прозрачный чат (заказчик): без панели и рамки; высота — доля
        // колонки из конфига (30 % исходно, 50 % после разворачивания).
        div()
            .flex()
            .flex_col()
            .gap_1()
            .h(gpui::relative(self.config.chat_height_pct.clamp(0.15, 0.9)))
            .min_h(px(120.0))
            .flex_shrink_0()
            .child(head)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_grow_1()
                    .min_h_0()
                    .child(self.chat_body(cx, pal)),
            )
    }
}

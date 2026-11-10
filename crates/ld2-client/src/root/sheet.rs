//! Окно персонажа gpui (M15/F25; порт листа M10.4): левая колонка —
//! шкала опыта, экипировка, атрибуты, статусы со временем; правая —
//! вкладки (параметры/навыки/профессии/репутация/квесты/гильдия/
//! инвентарь). Модалки предмета/улучшения/инкрустации — прежние.
//!
//! Правило дубликатов: атрибуты/статусы живут только в левой колонке —
//! одноимённых вкладок больше нет.
//!
//! Отличия от egui: инвентарь без drag&drop (действия в меню по ПКМ —
//! те же, что в контекстном меню egui), тултипы заменены модалкой
//! описания, combobox обучения — ряд кнопок. Состояние вкладки —
//! `Root::sheet_tab` (персистентно в config, ФР-19).

use ely::overlays::Dialog;
use gpui::{
    div, prelude::*, px, rgb, rgba, AnyElement, Context, Div, DragMoveEvent, EmptyView,
    MouseButton, SharedString, Window,
};
use ld2_core::l10n::{format_duration, t, tf, Key, Lang};
use ld2_core::store::{
    crime_kind_title, filter_active_statuses, local_epoch, Act, Screen, UpgradeView,
};
use ld2_protocol::cmd::{
    CharXpInvest, CraftLearn, CraftRecipes, CraftStart, CrimeLog, GuildBrowse, GuildLeave,
    GuildRankSet, ItemDrop, ItemEquip, ItemUnequip, ItemUse, QuestAbandon, QuestAccept,
    QuestTurnIn, SocketExtract, SocketInsert, UpgradePreview, UpgradeStart,
};
use ld2_protocol::types::{CharacterState, ItemSnapshot, QuestPhase, Severity, SkillSnapshot};

use crate::palette::Palette;
use ely::buttons::IconButton;
use ely::forms::{Choice, Input, Select};
use ely::layout::ScrollArea;
use ely::menus::{ContextMenu, Menu, MenuItem};
use ely::navigation::Tabs;
use ely::primitives::{IconName, Tooltip};

use super::{
    act_btn, char_card_origin, clamp_char_size, scrim_closes_on_up, scrollable_tab, CharDialogDrag,
    CharDialogResize, Root,
};

/// Вкладки окна персонажа (M15/F25; персистентно строкой в config).
/// Порядок — как в правой колонке окна (спека пользователя).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SheetTab {
    /// Производные характеристики — наполняется в F26.
    Params,
    Skills,
    Professions,
    /// Репутация фракций — наполняется в F27.
    Reputation,
    Quests,
    Guild,
    Inventory,
}

impl SheetTab {
    fn all() -> [(SheetTab, Key); 7] {
        [
            (SheetTab::Params, Key::SheetParams),
            (SheetTab::Skills, Key::SheetSkills),
            (SheetTab::Professions, Key::ProfessionsTab),
            (SheetTab::Reputation, Key::SheetReputation),
            (SheetTab::Quests, Key::QuestsTab),
            (SheetTab::Guild, Key::GuildTab),
            (SheetTab::Inventory, Key::SheetInventory),
        ]
    }

    /// M14/F12: иконка вкладки (Lucide-бандл Ely) — метка не меняется.
    pub(crate) fn icon(self) -> IconName {
        match self {
            SheetTab::Params => IconName::Gauge,
            SheetTab::Skills => IconName::Zap,
            SheetTab::Professions => IconName::Wrench,
            SheetTab::Reputation => IconName::Shield,
            SheetTab::Quests => IconName::ListChecks,
            SheetTab::Guild => IconName::Flag,
            SheetTab::Inventory => IconName::Package,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SheetTab::Params => "params",
            SheetTab::Skills => "skills",
            SheetTab::Professions => "professions",
            SheetTab::Reputation => "reputation",
            SheetTab::Quests => "quests",
            SheetTab::Guild => "guild",
            SheetTab::Inventory => "inventory",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "skills" => SheetTab::Skills,
            "professions" => SheetTab::Professions,
            "reputation" => SheetTab::Reputation,
            "quests" => SheetTab::Quests,
            "guild" => SheetTab::Guild,
            "inventory" => SheetTab::Inventory,
            // M15/F25: старые значения удалённых вкладок (attrs/statuses)
            // из сохранённого конфига → «Параметры» (ближайшая инфо-вкладка).
            "attrs" | "statuses" => SheetTab::Params,
            _ => SheetTab::Params,
        }
    }
}

/// M15/F28: группы навыков в порядке GDD §3 (A–F); ид — значение
/// `skills.skill_group` из контента, ключ — заголовок группы.
const SKILL_GROUPS: [(&str, Key); 6] = [
    ("weapon", Key::SkillGroupWeapon),
    ("defense", Key::SkillGroupDefense),
    ("magic", Key::SkillGroupMagic),
    ("stealth", Key::SkillGroupStealth),
    ("practice", Key::SkillGroupPractice),
    ("social", Key::SkillGroupSocial),
];

/// Демо-каталог профессий для обучения (skills.md §5; сервер валидирует).
const LEARN_CATALOG: [(&str, Key); 7] = [
    ("blacksmith", Key::ProfBlacksmith),
    ("alchemist", Key::ProfAlchemist),
    ("woodcarver", Key::ProfWoodcarver),
    ("tailor", Key::ProfTailor),
    ("cook", Key::ProfCook),
    ("herbalist", Key::ProfHerbalist),
    ("hunter", Key::ProfHunter),
];

impl Root {
    /// M11.2: диалоги листа (ФР-07/09/10): предмет/улучшение/инкрустация.
    pub(crate) fn sheet_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if let Some(detail) = self.store.item_detail.clone() {
            let name = detail["name"].as_str().unwrap_or("?").to_string();
            let body = self.item_view(cx, pal, detail).into_any_element();
            let close = self.dismiss(cx, |r| r.store.item_detail = None);
            return Some(
                Dialog::new("ld2-item", name, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        if let Some(view) = self.store.upgrade.clone() {
            let title = view.item_name.clone();
            let body = self.upgrade_view(cx, pal, &view).into_any_element();
            let close = self.dismiss(cx, |r| r.store.upgrade = None);
            return Some(
                Dialog::new("ld2-upgrade", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        if let Some(staff_uid) = self.store.socketing.clone() {
            let title = t(self.lang, Key::SocketingTitle).to_string();
            let body = self.socket_view(cx, pal, staff_uid).into_any_element();
            let close = self.dismiss(cx, |r| r.store.close_socketing());
            return Some(
                Dialog::new("ld2-socket", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        None
    }

    /// M14/F17: карточка информации о навыке (иконка ⓘ во вкладке
    /// «Навыки»): описание из контента, ранг и опыт. Навык исчез
    /// (смена персонажа/экрана) → карточка закрывается.
    pub(crate) fn skill_info_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let id = self.skill_info.clone()?;
        let info = self.store.snapshot.as_ref().and_then(|snap| {
            let skill = snap.char.skills.iter().find(|s| s.id == id)?;
            Some((
                skill.name.clone().unwrap_or_else(|| skill.id.clone()),
                skill.description.clone().unwrap_or_default(),
                skill.rank,
                skill.xp,
                skill.xp_to_next,
            ))
        });
        let Some((name, desc, rank, xp, next)) = info else {
            self.skill_info = None;
            return None;
        };
        let lang = self.lang;
        let close = self.dismiss(cx, |r| r.skill_info = None);
        let mut body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().child(t(lang, Key::RankFmt).replace("{r}", &rank.to_string())))
            .child(bar(
                if next > 0 {
                    xp as f32 / next as f32
                } else {
                    0.0
                },
                format!("{xp}/{next}"),
                pal.accent,
                pal,
            ));
        if !desc.is_empty() {
            body = body.child(div().child(desc));
        }
        Some(
            Dialog::new(format!("ld2-skill-info-{id}"), name, close)
                .child(body)
                .into_any_element(),
        )
    }

    // ── каркас окна ───────────────────────────────────────────────────────

    /// M15/F25b: **окно персонажа** — всплывающий диалог поверх игры
    /// (решение пользователя): скрим Ely `Backdrop` (клик мимо
    /// закрывает), карточка 760×560 по центру; Esc и ✕ в шапке карточки
    /// закрывают, кнопка в шапке/хоткей `c` — открывают. Игра видна
    /// позади (слой поверх `game_screen`, а не замена ему).
    pub(crate) fn character_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if !self.character_open || self.store.screen != Screen::Game {
            return None;
        }
        // Автозагрузки один раз (ФР-08/16 + M15/F25 журнал статусов).
        if self.store.snapshot.is_some() {
            // M15/F25: журнал нарушений нужен левой колонке окна —
            // грузим при открытии (вкладки «Статусы» больше нет).
            if !self.store.crime_log_loaded {
                self.store.crime_log_loaded = true;
                self.send_cmd(&CrimeLog { limit: Some(50) }, Act::CrimeLog);
            }
            if self.sheet_tab == SheetTab::Professions && !self.store.recipes_loaded {
                self.store.recipes_loaded = true;
                self.send_cmd(&CraftRecipes { profession: None }, Act::Craft);
            }
            if self.sheet_tab == SheetTab::Guild && !self.store.guild_browse_loaded {
                self.store.guild_browse_loaded = true;
                self.send_cmd(&GuildBrowse { query: None }, Act::GuildBrowse);
            }
        }

        let lang = self.lang;
        // Верх окна: заголовок + ✕ (дублирует Esc и кнопку в шапке).
        // M15/F25c: шапка — drag-регион (захват указателя gpui:
        // события идут, даже когда курсор ушёл за карточку).
        let top = {
            let drag_this = cx.entity();
            div()
                .id("ld2-char-titlebar")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .cursor_grab()
                // M15/F25e: жест начат на шапке — scrim его не закроет.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|root, _, _, _| {
                        root.char_scrim_down = false;
                    }),
                )
                .child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(t(lang, Key::BtnCharacter)),
                )
                .child(
                    IconButton::new("ld2-char-close", IconName::X)
                        .tooltip(t(lang, Key::BtnClose))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.character_open = false;
                            cx.notify();
                        })),
                )
                // Захват: запоминаем смещение указателя внутри шапки
                // (конструктор on_drag получает offset от gpui).
                .on_drag(CharDialogDrag, move |_value, offset, _window, acx| {
                    drag_this.update(acx, |root, cx| {
                        root.char_drag_offset = Some((f32::from(offset.x), f32::from(offset.y)));
                        cx.notify();
                    });
                    acx.new(|_| EmptyView)
                })
                .on_drag_move(cx.listener(
                    move |root, event: &DragMoveEvent<CharDialogDrag>, window, cx| {
                        let Some(offset) = root.char_drag_offset else {
                            return;
                        };
                        let position = event.event.position;
                        let viewport = window.viewport_size();
                        let (w, h) = root.char_dialog_size;
                        let base_x = ((f32::from(viewport.width) - w) / 2.0).max(0.0);
                        let base_y = ((f32::from(viewport.height) - h) / 2.0).max(0.0);
                        // Шапка внутри padding-а карточки (12 px) — левый
                        // верхний угол карточки = указатель − offset − 12.
                        root.char_dialog_delta.0 = f32::from(position.x) - offset.0 - base_x - 12.0;
                        root.char_dialog_delta.1 = f32::from(position.y) - offset.1 - base_y - 12.0;
                        cx.notify();
                    },
                ))
        };

        // M15/F25c: уголок изменения размера (правый нижний).
        let resize_grip = {
            let grip_this = cx.entity();
            div()
                .id("ld2-char-resize")
                .absolute()
                .right_0()
                .bottom_0()
                .size(px(18.0))
                .rounded_md()
                .border_l_1()
                .border_t_1()
                .border_color(rgb(pal.border))
                .bg(rgb(pal.panel))
                .cursor_pointer()
                // M15/F25e: жест начат на уголке — scrim не закроет.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|root, _, _, _| {
                        root.char_scrim_down = false;
                    }),
                )
                .on_drag(CharDialogResize, move |_value, _offset, _window, acx| {
                    grip_this.update(acx, |root, cx| {
                        // Новый захват — новый якорь (иначе скачок по
                        // остатку старого).
                        root.char_resize_anchor = None;
                        cx.notify();
                    });
                    acx.new(|_| EmptyView)
                })
                .on_drag_move(cx.listener(
                    move |root, event: &DragMoveEvent<CharDialogResize>, window, cx| {
                        let pos = (
                            f32::from(event.event.position.x),
                            f32::from(event.event.position.y),
                        );
                        let (cur_w, cur_h) = root.char_dialog_size;
                        // Якорь — первый move (позицию захвата в конструктор
                        // on_drag не передаёт); кадр движения теряем.
                        let Some((ax, ay, w0, h0, dx0, dy0)) = root.char_resize_anchor else {
                            root.char_resize_anchor = Some((
                                pos.0,
                                pos.1,
                                cur_w,
                                cur_h,
                                root.char_dialog_delta.0,
                                root.char_dialog_delta.1,
                            ));
                            return;
                        };
                        let viewport = window.viewport_size();
                        let (nw, nh) = clamp_char_size(
                            w0 + (pos.0 - ax),
                            h0 + (pos.1 - ay),
                            f32::from(viewport.width),
                            f32::from(viewport.height),
                        );
                        // Якорим левый верхний угол: компенсируем
                        // центрирование Backdrop (resize от центра
                        // сдвигал бы оба края на Δ/2).
                        root.char_dialog_delta.0 = dx0 + (nw - w0) / 2.0;
                        root.char_dialog_delta.1 = dy0 + (nh - h0) / 2.0;
                        root.char_dialog_size = (nw, nh);
                        cx.notify();
                    },
                ))
        };

        let (w, h) = self.char_dialog_size;
        // M15/F25d: absolute-позиция от слоя (F14-подход): центр
        // вьюпорта + смещение — марджины центрирования давали лишь
        // половину сдвига («резиновый» drag).
        let viewport = window.viewport_size();
        let (left, top_xy) = char_card_origin(
            f32::from(viewport.width),
            f32::from(viewport.height),
            w,
            h,
            self.char_dialog_delta,
        );
        let card = div()
            .absolute()
            .left(px(left))
            .top(px(top_xy))
            .flex()
            .flex_col()
            .gap_2()
            .w(px(w))
            .h(px(h))
            .p(px(12.0))
            .rounded_md()
            .border_1()
            .border_color(rgb(pal.border))
            .bg(rgb(pal.panel))
            .child(top)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .flex_grow_1()
                    .min_h_0()
                    .child(self.character_left_column(cx, pal))
                    .child(self.character_tabs(cx, pal)),
            )
            .child(resize_grip);

        // M15/F25d: свой слой по F14-шаблону: scrim закрывает по
        // mouse-**up** — попытка перетаскивания (mousedown) физически
        // не может закрыть окно; Esc — наш глобальный обработчик.
        Some(
            div()
                .id("ld2-char-dialog-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(
                    div()
                        .id("ld2-char-scrim")
                        .size_full()
                        .bg(rgba(0x99000000))
                        // M15/F25e: запоминаем, что жест начат на scrim.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|root, _, _, _| {
                                root.char_scrim_down = true;
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|root, _, _, cx| {
                                let close =
                                    scrim_closes_on_up(root.char_scrim_down, cx.has_active_drag());
                                root.char_scrim_down = false;
                                if close {
                                    root.character_open = false;
                                    cx.notify();
                                }
                            }),
                        ),
                )
                .child(card)
                .into_any_element(),
        )
    }

    /// M15/F25: левая колонка окна — шкала опыта, экипировка,
    /// разделитель, атрибуты, разделитель, статусы со временем
    /// (+ журнал нарушений) — по спеке пользователя; скроллится.
    fn character_left_column(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let mut col = div().flex().flex_col().gap_2();
        col = col.child(self.character_xp_block(pal));
        col = col.child(self.equip_block(cx, pal));
        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        col = col.child(self.attrs_block(pal));
        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        col = col.child(self.statuses_block(cx, pal));
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(300.0))
            .min_h_0()
            .child(
                ScrollArea::new("ld2-char-left")
                    .vertical()
                    .flex_grow_1()
                    .min_h_0()
                    .child(col),
            )
    }

    /// M15/F25: шкала опыта — уровень и прогресс (данные F24),
    /// общий опыт и очки опыта; `level = None` (сервер без M15) —
    /// показываем только очки.
    fn character_xp_block(&self, pal: &Palette) -> Div {
        let lang = self.lang;
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_2()
            .rounded_md()
            .bg(rgb(pal.panel));
        let Some(snap) = self.store.snapshot.as_ref() else {
            return panel.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::Loading)),
            );
        };
        if let (Some(level), Some(xp), Some(next)) =
            (snap.char.level, snap.char.xp_in_level, snap.char.xp_to_next)
        {
            panel = panel.child(div().font_weight(gpui::FontWeight::BOLD).child(tf(
                lang,
                Key::CharLevel,
                &[("{n}", &level.to_string())],
            )));
            if next > 0 {
                panel = panel.child(bar(
                    xp as f32 / next as f32,
                    format!("{xp} / {next}"),
                    pal.accent,
                    pal,
                ));
            }
        }
        if let Some(total) = snap.char.total_xp {
            panel = panel.child(xp_kv_row(
                pal,
                t(lang, Key::CharXpTotal),
                &total.to_string(),
            ));
        }
        panel = panel.child(xp_kv_row(
            pal,
            t(lang, Key::CharXpPool),
            &snap.char.xp_pool.to_string(),
        ));
        panel
    }

    /// M15/F25: правая колонка окна — вкладки с автозагрузками.
    fn character_tabs(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let tab = self.sheet_tab;
        let mut col = div()
            .flex()
            .flex_col()
            .gap_1()
            .flex_grow_1()
            .min_h_0()
            .min_w_0();

        // M11.4: вкладки листа — Tabs Ely (выбор + клавиатурная навигация).
        let choices: Vec<Choice> = SheetTab::all()
            .iter()
            .map(|(candidate, key)| {
                Choice::new(candidate.as_str(), t(lang, *key)).icon(candidate.icon())
            })
            .collect();
        let this = cx.entity();
        col = col.child(
            Tabs::new("ld2-sheet-tabs", choices, tab.as_str()).on_change(
                move |value, _window, acx| {
                    let value = value.to_string();
                    this.update(acx, |root, cx| {
                        root.sheet_tab = SheetTab::parse(&value);
                        root.config.sheet_tab = value;
                        root.save_config();
                        cx.notify();
                    });
                },
            ),
        );
        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));

        if let Some(err) = &self.store.sheet_error {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }

        let body = match tab {
            // M15/F26: производные характеристики; заглушка до F27 — «Репутация».
            SheetTab::Params => self.tab_params(pal),
            SheetTab::Reputation => div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::CharComingSoon)),
            SheetTab::Skills => self.tab_skills(cx, pal),
            SheetTab::Inventory => self.tab_inventory(cx, pal),
            SheetTab::Quests => self.tab_quests(cx, pal),
            SheetTab::Professions => self.tab_professions(cx, pal),
            SheetTab::Guild => self.tab_guild(cx, pal),
        };
        // M14/F3: длинные списки (навыки/профессии с рецептами) —
        // прокручиваются, остальные вкладки ведут себя как раньше.
        col = col.child(match tab {
            SheetTab::Skills => scrollable_tab("ld2-skills-scroll", &self.skills_scroll, body),
            SheetTab::Params => scrollable_tab("ld2-params-scroll", &self.params_scroll, body),
            SheetTab::Professions => {
                scrollable_tab("ld2-professions-scroll", &self.professions_scroll, body)
            }
            SheetTab::Inventory => {
                scrollable_tab("ld2-inventory-scroll", &self.inventory_scroll, body)
            }
            _ => body,
        });
        col
    }

    /// M15/F26: вкладка «Параметры» — 16 производных характеристик
    /// (серверные `derived`; у сервера без M15 — заглушка).
    fn tab_params(&self, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.as_ref() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let Some(d) = snap.char.derived.as_ref() else {
            return div()
                .px_2()
                .py_1()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::CharComingSoon));
        };
        // Порядок строк — спецификация пользователя (16 позиций).
        let rows: [(&Key, String); 16] = [
            (&Key::ParAccuracy, format!("{}%", d.accuracy)),
            (
                &Key::ParDamage,
                format!("{}..{}", d.damage_min, d.damage_max),
            ),
            (&Key::ParCritChance, format!("{}%", d.crit_chance)),
            (&Key::ParCritDamage, format!("{}%", d.crit_damage)),
            (&Key::ParArmor, d.armor.to_string()),
            (&Key::ParDodge, format!("{}%", d.dodge)),
            (&Key::ParParry, format!("{}..{}", d.parry_min, d.parry_max)),
            (&Key::ParParryChance, format!("{}%", d.parry_chance)),
            (&Key::ParBlock, format!("{}..{}", d.block_min, d.block_max)),
            (&Key::ParBlockChance, format!("{}%", d.block_chance)),
            (&Key::ParMagicDefense, d.magic_defense.to_string()),
            (&Key::ParMagicDodge, format!("{}%", d.magic_dodge)),
            (&Key::ParMagicResist, format!("{}%", d.magic_resist)),
            (&Key::ParMagicDispel, format!("{}%", d.magic_dispel)),
            (
                &Key::ParMagicCritDamage,
                format!("{}%", d.magic_crit_damage),
            ),
            (
                &Key::ParMagicCritChance,
                format!("{}%", d.magic_crit_chance),
            ),
        ];
        let mut col = div().flex().flex_col().gap_1();
        for (label, value) in rows {
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .child(div().text_color(rgb(pal.muted)).child(t(lang, *label)))
                    .child(div().font_weight(gpui::FontWeight::BOLD).child(value)),
            );
        }
        col
    }

    /// Отправка команды листа (паритет egui `send`).
    fn send_cmd<C: ld2_protocol::Command>(&mut self, cmd: &C, act: Act) {
        match self.transport.send(cmd) {
            Ok(_) => self.store.on_sheet_sent(act),
            Err(e) => self.store.sheet_error = Some(format!("нет связи: {e}")),
        }
    }

    // ── трекер квестов (ФР-12) ────────────────────────────────────────────

    pub(crate) fn tracker_block(&self, pal: &Palette) -> Option<Div> {
        if self.store.quest_tracker.is_empty() {
            return None;
        }
        let lang = self.lang;
        let snap = self.store.snapshot.as_ref()?;
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
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::TrackerLabel)),
            );
        for id in &self.store.quest_tracker {
            if let Some(q) = snap.char.quests.iter().find(|q| &q.id == id) {
                block = block.child(div().text_size(px(13.0)).child(format!(
                    "{} — {}",
                    q.title.clone().unwrap_or_else(|| q.id.clone()),
                    goals_summary(q)
                )));
            }
        }
        Some(block)
    }

    // ── вкладка «Навыки» (ФР-04) ──────────────────────────────────────────

    fn tab_skills(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let pool = snap.char.xp_pool;
        let mut col = div().flex().flex_col().gap_2().child(
            div().font_weight(gpui::FontWeight::BOLD).child(format!(
                "{}: {}",
                t(lang, Key::XpPoolLabel),
                pool
            )),
        );
        // M15/F28: навыки по группам GDD §3 (A–F) — заголовок группы,
        // если в снапшоте есть хоть один её навык.
        for (gid, key) in SKILL_GROUPS {
            let group: Vec<&SkillSnapshot> = snap
                .char
                .skills
                .iter()
                .filter(|s| s.group.as_deref() == Some(gid))
                .collect();
            if group.is_empty() {
                continue;
            }
            col = col.child(
                div()
                    .mt(px(6.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(t(lang, key)),
            );
            for skill in group {
                col = col.child(self.skill_row(cx, pal, skill, pool));
            }
        }
        // Строки без известной группы — в конце: старый сервер (group =
        // None) или новая группа контента без ключа l10n.
        let rest: Vec<&SkillSnapshot> = snap
            .char
            .skills
            .iter()
            .filter(|s| {
                s.group
                    .as_deref()
                    .is_none_or(|g| !SKILL_GROUPS.iter().any(|(id, _)| *id == g))
            })
            .collect();
        for skill in rest {
            col = col.child(self.skill_row(cx, pal, skill, pool));
        }
        col
    }

    /// M15/F28: строка навыка (перенос из таба) — название/ранг,
    /// тултип описания, ⓘ-карточка, прогресс и вложение XP из пула.
    fn skill_row(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        skill: &SkillSnapshot,
        pool: i64,
    ) -> impl gpui::IntoElement {
        let lang = self.lang;
        let name = skill.name.clone().unwrap_or_else(|| skill.id.clone());
        let id = skill.id.clone();
        let rank = skill.rank;
        // M14/F17: описание навыка — в тултипе строки; ⓘ открывает
        // карточку с полной информацией.
        let desc = skill.description.clone().unwrap_or_default();
        let info_id = id.clone();
        let mut row = div().id(format!("ld2-skill-{id}"));
        if !desc.is_empty() {
            row = row.tooltip(Tooltip::text(desc));
        }
        let mut row = row
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(pal.panel))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .child(div().child(name))
                    .child(div().text_color(rgb(pal.muted)).child(format!("·{rank}")))
                    .child(div().flex_grow_1())
                    .child(
                        IconButton::new(format!("ld2-skill-info-{id}"), IconName::Info)
                            .tooltip(t(lang, Key::SkillInfoBtn))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.skill_info = Some(info_id.clone());
                                cx.notify();
                            })),
                    ),
            )
            .child(bar(
                if skill.xp_to_next > 0 {
                    skill.xp as f32 / skill.xp_to_next as f32
                } else {
                    0.0
                },
                format!("{}/{}", skill.xp, skill.xp_to_next),
                pal.accent,
                pal,
            ));
        // Вложение очков из пула (D3/D4, ФР-04).
        if pool > 0 {
            let mut buttons = div().flex().flex_row().gap_1();
            for (label, points) in [("+1", 1_i64), ("+5", 5_i64)] {
                let sid = id.clone();
                buttons =
                    buttons.child(act_btn(cx, label, pal.panel, pal.text, move |this, cx| {
                        this.invest(&sid, points.min(pool));
                        let _ = cx;
                    }));
            }
            let sid = id.clone();
            buttons = buttons.child(act_btn(
                cx,
                t(lang, Key::MaxLabel),
                pal.panel,
                pal.text,
                move |this, cx| {
                    this.invest(&sid, pool);
                    let _ = cx;
                },
            ));
            row = row.child(buttons);
        }
        row
    }

    fn invest(&mut self, skill_id: &str, points: i64) {
        let cmd = CharXpInvest {
            skill_id: skill_id.to_string(),
            points,
        };
        self.send_cmd(&cmd, Act::Invest);
    }

    // ── вкладка «Атрибуты» ────────────────────────────────────────────────

    /// M15/F25: атрибуты — середина левой колонки (перенос из вкладки
    /// «Атрибуты» по правилу дубликатов) — с заголовком секции.
    fn attrs_block(&self, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.as_ref() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let mut col = div().flex().flex_col().gap_1().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::SheetAttrs)),
        );
        for (key, value) in &snap.char.attrs {
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .px_2()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .child(div().text_color(rgb(pal.muted)).child(key.clone()))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(value.to_string()),
                    ),
            );
        }
        col
    }

    // ── вкладка «Инвентарь» (ФР-07) ───────────────────────────────────────

    /// M15/F25: экипировка — верх левой колонки (перенос из «Инвентаря»
    /// по правилу дубликатов): слоты с действиями снятия/описания.
    fn equip_block(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let equip_slots: Vec<String> = self
            .store
            .session
            .as_ref()
            .map(|s| s.inventory.equip_slots.clone())
            .unwrap_or_default();

        let mut col = div().flex().flex_col().gap_2();

        // Слоты экипировки (ФР-07): вместо drag&drop — действия в строке.
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::EquipLabel)),
        );
        for slot in &equip_slots {
            let uid = snap.char.equip.get(slot).cloned().flatten();
            let item = uid
                .as_ref()
                .and_then(|u| snap.char.inventory.iter().find(|i| i.uid == *u));
            let mut row = div()
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel))
                .child(
                    div()
                        .w(px(72.0))
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(slot.clone()),
                );
            match item {
                Some(item) => {
                    let color = rarity_color(&item.rarity, pal);
                    row = row.child(div().flex_grow_1().text_color(rgb(color)).child(format!(
                        "{} {}",
                        item.name,
                        badges(item)
                    )));
                    let slot = slot.clone();
                    row = row.child(act_btn(
                        cx,
                        t(lang, Key::MenuUnequip),
                        pal.panel,
                        pal.text,
                        move |this, cx| {
                            let cmd = ItemUnequip { slot: slot.clone() };
                            this.send_cmd(&cmd, Act::Unequip);
                            let _ = cx;
                        },
                    ));
                    let detail = serde_json::to_value(item).unwrap_or_default();
                    row = row.child(act_btn(
                        cx,
                        t(lang, Key::MenuDesc),
                        pal.panel,
                        pal.text,
                        move |this, cx| {
                            this.store.item_detail = Some(detail.clone());
                            let _ = cx;
                        },
                    ));
                }
                None => {
                    row = row.child(
                        div()
                            .flex_grow_1()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(t(lang, Key::EmptySlot)),
                    );
                }
            }
            col = col.child(row);
        }

        col
    }

    fn tab_inventory(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let mut col = div().flex().flex_col().gap_2();

        // Сетка инвентаря (ФР-07): клик — описание, ПКМ — действия.
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::SheetInventory)),
        );
        if snap.char.inventory.is_empty() {
            // M14/F3: вместо многоточия — понятное пустое состояние.
            col = col.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::InventoryEmpty)),
            );
        }
        for item in &snap.char.inventory {
            let uid = item.uid.clone();
            let detail = serde_json::to_value(item).unwrap_or_default();
            let tile = div()
                .id(format!("ld2-item-{uid}"))
                .tooltip(Tooltip::text(format!(
                    "{} · {} · +{}",
                    item.name, item.rarity, item.upgrade
                )))
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(pal.panel))
                .border_1()
                .border_color(rgb(pal.border))
                .cursor_pointer()
                .hover(move |s| s.bg(rgb(pal.selection)))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener({
                        let detail = detail.clone();
                        move |this, _, _, cx| {
                            // Левый клик — описание (паритет тултипа egui).
                            this.store.item_detail = Some(detail.clone());
                            cx.notify();
                        }
                    }),
                )
                .child(
                    div()
                        .text_color(rgb(rarity_color(&item.rarity, pal)))
                        .child(format!("{} {}", item.name, badges(item))),
                );
            // M11.4: ПКМ по предмету — контекстное меню Ely (полный набор
            // действий egui-паритета; инлайн-раскрытие M10 снято).
            col = col.child(
                ContextMenu::new(
                    format!("ld2-item-ctx-{uid}"),
                    self.item_actions(cx, pal, &snap.char, item),
                )
                .child(tile),
            );
        }
        col
    }

    /// Действия предмета — контекстное меню Ely (ФР-07/09/10, egui-паритет).
    fn item_actions(
        &mut self,
        cx: &mut Context<'_, Self>,
        _pal: &Palette,
        ch: &CharacterState,
        item: &ItemSnapshot,
    ) -> Menu {
        let lang = self.lang;
        let mut menu = Menu::new();

        // Использовать (свиток/зелье/…).
        let uid = item.uid.clone();
        menu = menu.item(menu_item(cx, t(lang, Key::MenuUse), move |this, _cx| {
            let cmd = ItemUse {
                item_uid: uid.clone(),
                target_id: None,
            };
            this.send_cmd(&cmd, Act::Drop);
        }));

        // Экипировать (гейт клиента по типу; сервер — истина, §7).
        if let Some(slot) = slot_for(&item.item_type) {
            let equipped = ch.equip.values().flatten().any(|u| u == &item.uid);
            if !equipped {
                let uid = item.uid.clone();
                let slot = slot.to_string();
                menu = menu.item(menu_item(cx, t(lang, Key::MenuEquip), move |this, _cx| {
                    let cmd = ItemEquip {
                        item_uid: uid.clone(),
                        slot: slot.clone(),
                    };
                    this.send_cmd(&cmd, Act::Equip);
                }));
            }
        }

        // Улучшение (ФР-10): до максимума из конфига сервера.
        let max_level = self
            .store
            .session
            .as_ref()
            .map(|s| s.upgrade.max_level)
            .unwrap_or(9) as i32;
        let upgradable = matches!(item.item_type.as_str(), "weapon" | "armor" | "staff");
        if upgradable && item.upgrade < max_level {
            let uid = item.uid.clone();
            let name = item.name.clone();
            menu = menu.item(menu_item(cx, t(lang, Key::UpgradeBtn), move |this, _cx| {
                this.store.open_upgrade(uid.clone(), name.clone());
                let cmd = UpgradePreview {
                    item_uid: uid.clone(),
                };
                this.send_cmd(&cmd, Act::Preview);
            }));
        }

        // Инкрустация (ФР-09): только посох с сокетами.
        if item.item_type == "staff" && item.sockets.is_some() {
            let uid = item.uid.clone();
            menu = menu.item(menu_item(
                cx,
                t(lang, Key::BtnSocketing),
                move |this, _cx| {
                    this.store.open_socketing(uid.clone());
                },
            ));
        }

        // Описание / Выбросить.
        let detail = serde_json::to_value(item).unwrap_or_default();
        menu = menu.item(menu_item(cx, t(lang, Key::MenuDesc), move |this, _cx| {
            this.store.item_detail = Some(detail.clone());
        }));
        let uid = item.uid.clone();
        menu = menu.item(menu_item(cx, t(lang, Key::MenuDrop), move |this, _cx| {
            let cmd = ItemDrop {
                item_uid: uid.clone(),
                qty: None,
            };
            this.send_cmd(&cmd, Act::Drop);
        }));
        menu
    }

    // ── вкладка «Статусы» (ФР-11, M6) ────────────────────────────────────

    /// M15/F25: статусы со временем + журнал нарушений — низ левой
    /// колонки (перенос из вкладки «Статусы» по правилу дубликатов).
    fn statuses_block(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        // Отсчёт по серверным срокам: now = локальное + offset (§1.1).
        let now = local_epoch() + self.store.time_offset;
        let active = filter_active_statuses(&snap.char.crime.statuses, now);
        let mut col = div().flex().flex_col().gap_1().child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::StatusesTab)),
        );

        if active.is_empty() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoStatuses)),
            );
        }
        for status in &active {
            let title = crime_kind_title(&self.store, &status.kind);
            let left = status.expires_at - now;
            col = col.child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        div()
                            .text_color(rgb(severity_rgb(status.severity)))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_color(rgb(pal.muted))
                            .child(format_duration(lang, left)),
                    ),
            );
        }

        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::CrimeJournal)),
        );
        col = col.child(act_btn(
            cx,
            t(lang, Key::ReloadJournal),
            pal.panel,
            pal.text,
            |this, cx| {
                let cmd = CrimeLog { limit: Some(50) };
                this.send_cmd(&cmd, Act::CrimeLog);
                let _ = cx;
            },
        ));
        if self.store.crime_log.is_empty() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::JournalEmpty)),
            );
        }
        for entry in self.store.crime_log.iter().rev().take(60) {
            let title = crime_kind_title(&self.store, &entry.kind);
            col = col.child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(pal.muted))
                    .child(format!(
                        "{title}: +{} · {} {}",
                        format_duration(lang, entry.delta_s),
                        t(lang, Key::TotalLabel),
                        format_duration(lang, entry.total_s)
                    )),
            );
        }
        col
    }

    // ── вкладка «Квесты» (ФР-12) ──────────────────────────────────────────

    fn tab_quests(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        if snap.char.quests.is_empty() {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::NoQuests));
        }
        let mut col = div().flex().flex_col().gap_2();
        for quest in &snap.char.quests {
            let tracked = self.store.quest_tracker.contains(&quest.id);
            let (state_key, color) = match quest.state {
                QuestPhase::Available => (Key::StateAvailable, 0x98_c3_79),
                QuestPhase::Active => (Key::StateActive, 0xab_b2_bf),
                QuestPhase::TurnIn => (Key::StateTurnIn, 0xd1_9a_66),
                QuestPhase::Done => (Key::StateDone, 0x61_af_ef),
            };
            let mut card = div()
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
                        .flex()
                        .flex_row()
                        .gap_2()
                        .items_center()
                        .child(div().text_color(rgb(color)).child(t(lang, state_key)))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(quest.title.clone().unwrap_or_else(|| quest.id.clone())),
                        ),
                );

            // Трекер (ФР-12): максимум 3, переключение.
            let qid = quest.id.clone();
            card = card.child(act_btn(
                cx,
                if tracked {
                    t(lang, Key::TrackedBtn)
                } else {
                    t(lang, Key::TrackBtn)
                },
                if tracked { pal.accent } else { pal.panel },
                pal.text,
                move |this, cx| {
                    this.store.toggle_quest_tracker(&qid);
                    let _ = cx;
                },
            ));

            // Кнопки состояния (§7).
            let mut actions = div().flex().flex_row().gap_1();
            match quest.state {
                QuestPhase::Available => {
                    let qid = quest.id.clone();
                    actions = actions.child(act_btn(
                        cx,
                        t(lang, Key::BtnAccept),
                        pal.accent,
                        pal.accent_text,
                        move |this, cx| {
                            let cmd = QuestAccept {
                                quest_id: qid.clone(),
                            };
                            this.send_cmd(&cmd, Act::Quest);
                            let _ = cx;
                        },
                    ));
                }
                QuestPhase::Active => {
                    let qid = quest.id.clone();
                    actions = actions.child(act_btn(
                        cx,
                        t(lang, Key::BtnAbandon),
                        pal.panel,
                        pal.danger,
                        move |this, cx| {
                            let cmd = QuestAbandon {
                                quest_id: qid.clone(),
                            };
                            this.send_cmd(&cmd, Act::Quest);
                            let _ = cx;
                        },
                    ));
                }
                QuestPhase::TurnIn => {
                    let qid = quest.id.clone();
                    actions = actions.child(act_btn(
                        cx,
                        t(lang, Key::BtnTurnIn),
                        pal.accent,
                        pal.accent_text,
                        move |this, cx| {
                            let cmd = QuestTurnIn {
                                quest_id: qid.clone(),
                            };
                            this.send_cmd(&cmd, Act::Quest);
                            let _ = cx;
                        },
                    ));
                }
                QuestPhase::Done => {}
            }
            card = card.child(actions);

            if !quest.goals.is_empty() {
                card = card.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("• {}", goals_summary(quest))),
                );
            }
            if let Some(reward) = &quest.reward_preview {
                if let Some(xp) = reward.xp {
                    card = card.child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(pal.muted))
                            .child(format!("{}: {xp} XP", t(lang, Key::RewardLabel))),
                    );
                }
            }
            col = col.child(card);
        }
        col
    }

    // ── вкладка «Профессии» (ФР-08) ───────────────────────────────────────

    fn tab_professions(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .text_color(rgb(pal.muted))
                .child(t(lang, Key::Loading));
        };
        let mut col = div().flex().flex_col().gap_2();

        // Изученные профессии: ранги и XP (ФР-08).
        for prof in &snap.char.professions {
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(div().child(prof_label(lang, &prof.id)))
                            .child(div().text_color(rgb(pal.muted)).child(
                                t(lang, Key::RankFmt).replace("{r}", &prof.rank.to_string()),
                            )),
                    )
                    .child(bar(
                        if prof.xp_to_next > 0 {
                            prof.xp as f32 / prof.xp_to_next as f32
                        } else {
                            0.0
                        },
                        format!("{}/{}", prof.xp, prof.xp_to_next),
                        pal.accent,
                        pal,
                    )),
            );
        }

        // M11.3: обучение — Select Ely + подтверждение (отклонение
        // «ряд кнопок вместо combobox» снят).
        let available: Vec<(&'static str, Key)> = LEARN_CATALOG
            .iter()
            .copied()
            .filter(|(id, _)| !snap.char.professions.iter().any(|p| p.id == *id))
            .collect();
        if !available.is_empty() {
            let choices: Vec<Choice> = available
                .iter()
                .map(|(id, key)| Choice::new(*id, t(lang, *key)))
                .collect();
            let pick = self.learn_pick.clone();
            let this = cx.entity();
            let mut row = div().flex().flex_row().gap_2().items_center();
            row = row.child(div().flex_grow_1().child({
                let this = this.clone();
                let sel =
                    Select::new("ld2-learn-prof", choices).placeholder(t(lang, Key::LearnLabel));
                match pick {
                    Some(value) => sel.selected(value),
                    None => sel,
                }
                .on_change(move |value, _window, acx| {
                    let value = value.to_string();
                    this.update(acx, |root, _cx| root.learn_pick = Some(value));
                })
            }));
            if let Some(id) = self.learn_pick.clone() {
                row = row.child(act_btn(
                    cx,
                    t(lang, Key::LearnLabel),
                    pal.accent,
                    pal.accent_text,
                    move |this, cx| {
                        let cmd = CraftLearn {
                            profession_id: id.clone(),
                        };
                        this.send_cmd(&cmd, Act::Learn);
                        let _ = cx;
                    },
                ));
            }
            col = col.child(row);
        }

        if let Some(err) = &self.store.sheet_error.clone() {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }

        col = col.child(div().h(px(1.0)).bg(rgb(pal.border)));
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(t(lang, Key::RecipesLabel)),
        );
        if self.store.recipes.is_empty() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::NoRecipes)),
            );
        }

        // Рецепты: требования, ингредиенты (красные при нехватке — ФР-08).
        let recipes = self.store.recipes.clone();
        for recipe in &recipes {
            let recipe_id = recipe["id"].as_str().unwrap_or_default().to_string();
            let name = recipe["name"].as_str().unwrap_or("?").to_string();
            let profession = recipe["profession"].as_str().unwrap_or("").to_string();
            let rank = recipe["rank"].as_i64().unwrap_or(0);
            let station = recipe["station_label"].as_str().unwrap_or("").to_string();

            let mut ingredients_ok = true;
            let mut ingredient_lines: Vec<(String, bool)> = Vec::new();
            if let Some(inputs) = recipe["inputs"].as_array() {
                for input in inputs {
                    let need_type = input["type"].as_str().unwrap_or_default();
                    let need_qty = input["qty"].as_i64().unwrap_or(1);
                    let have: i64 = snap
                        .char
                        .inventory
                        .iter()
                        .filter(|i| i.item_type == need_type)
                        .map(|i| i.qty.unwrap_or(1))
                        .sum();
                    let ok = have >= need_qty;
                    if !ok {
                        ingredients_ok = false;
                    }
                    ingredient_lines.push((format!("{need_type}: {have}/{need_qty}"), ok));
                }
            }

            let mut card = div()
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
                        .flex()
                        .flex_row()
                        .gap_2()
                        .items_center()
                        .child(div().child(name.clone()))
                        .child(div().text_size(px(12.0)).text_color(rgb(pal.muted)).child(
                            format!(
                                "· {} · {}",
                                prof_label(lang, &profession),
                                t(lang, Key::RankFmt).replace("{r}", &rank.to_string())
                            ),
                        )),
                );

            // Крафт: кнопка активна при полном наборе и вне ожидания (§7).
            let pending = self.store.pending == Some(Act::Craft);
            if ingredients_ok && !pending {
                card = card.child(act_btn(
                    cx,
                    t(lang, Key::BtnCraft),
                    pal.accent,
                    pal.accent_text,
                    move |this, cx| {
                        let cmd = CraftStart {
                            recipe_id: recipe_id.clone(),
                            qty: 1,
                        };
                        this.send_cmd(&cmd, Act::Craft);
                        let _ = cx;
                    },
                ));
            } else {
                card = card.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(pal.border))
                        .text_color(rgb(pal.muted))
                        .child(t(lang, Key::BtnCraft)),
                );
            }

            if !station.is_empty() {
                card = card.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(station),
                );
            }
            for (line, ok) in ingredient_lines {
                card = card.child(if ok {
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("• {line}"))
                } else {
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.danger))
                        .child(format!("• {}: {}", t(lang, Key::MissingIngredients), line))
                });
            }
            col = col.child(card);
        }
        col
    }

    // ── вкладка «Гильдия» (ФР-16, M8) ─────────────────────────────────────

    fn tab_guild(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
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
            if let Some(err) = &self.store.sheet_error.clone() {
                col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
            }
            return col;
        };

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
                    .text_size(px(13.0))
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
                        .text_size(px(12.0))
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
                        .text_size(px(11.0))
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
                    .text_size(px(13.0))
                    .child(t(lang, Key::GuildLogLbl)),
            );
            for entry in self.store.guild_log.iter().rev().take(5) {
                col = col.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("• {}", entry.text)),
                );
            }
        }
        if let Some(err) = &self.store.sheet_error.clone() {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }
        col
    }

    // ── модалки (паритет egui-окон; вызываются из game_screen) ────────────

    /// Описание предмета (ФР-07).
    pub(crate) fn item_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        detail: serde_json::Value,
    ) -> Div {
        let lang = self.lang;
        let rarity = detail["rarity"].as_str().unwrap_or("common");
        let mut col = div().flex().flex_col().gap_1().child(
            div()
                .text_color(rgb(rarity_color(rarity, pal)))
                .child(format!(
                    "{rarity} · {} · +{}",
                    detail["type"].as_str().unwrap_or("?"),
                    detail["upgrade"].as_i64().unwrap_or(0)
                )),
        );
        if let Some(stats) = detail["stats"].as_object() {
            for (key, value) in stats {
                col = col.child(div().child(format!("{key}: {value}")));
            }
        }
        if let Some(sockets) = detail["sockets"].as_array() {
            for socket in sockets {
                if let Some(rune) = socket["rune"].as_object() {
                    col = col.child(div().child(format!(
                        "◈ {}: {}/{}",
                        rune["name"], rune["charges"], rune["max"]
                    )));
                } else {
                    col = col.child(div().text_color(rgb(pal.muted)).child("○ —"));
                }
            }
        }
        if let Some(value) = detail["gold_value"].as_i64() {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(format!("{value} зол.")),
            );
        }
        col.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, cx| {
                this.store.item_detail = None;
                let _ = cx;
            },
        ))
    }

    /// Улучшение (ФР-10): «сейчас +N → после +M», шанс, стоимость (§7).
    pub(crate) fn upgrade_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        view: &UpgradeView,
    ) -> Div {
        let lang = self.lang;
        let pending = self.store.pending == Some(Act::Upgrade);
        let mut col = div().flex().flex_col().gap_1();
        let Some(preview) = view.preview.clone() else {
            col = col.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(t(lang, Key::Loading)),
            );
            col = col.child(act_btn(
                cx,
                t(lang, Key::Exit),
                pal.panel,
                pal.text,
                |this, cx| {
                    this.store.close_upgrade();
                    let _ = cx;
                },
            ));
            return col;
        };

        let current = self
            .store
            .snapshot
            .as_ref()
            .and_then(|s| {
                s.char
                    .inventory
                    .iter()
                    .find(|i| i.uid == view.item_uid)
                    .map(|i| i.upgrade)
            })
            .unwrap_or(0);
        let delta = preview["delta"]["upgrade"].as_str().unwrap_or("+?");
        col = col.child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(format!("+{current} → {delta}")),
        );
        if let Some(chance) = preview["chance"].as_f64() {
            col = col.child(div().text_color(rgb(pal.muted)).child(format!(
                "{}: {:.0}%",
                t(lang, Key::ChanceLabel),
                chance * 100.0
            )));
        }
        if let Some(gold) = preview["cost"]["gold"].as_i64() {
            col = col.child(div().text_color(rgb(pal.muted)).child(format!(
                "{}: {} зол.",
                t(lang, Key::CostLabel),
                gold
            )));
        }
        if let Some(materials) = preview["cost"]["materials"].as_object() {
            for (name, qty) in materials {
                col = col.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(pal.muted))
                        .child(format!("  • {name}: {qty}")),
                );
            }
        }
        if let Some(err) = &self.store.sheet_error.clone() {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }

        let preview_id = preview["preview_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let can_start = !preview_id.is_empty() && !pending;
        let mut row = div().flex().flex_row().gap_2().mt(px(8.0));
        row = row.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, cx| {
                this.store.close_upgrade();
                let _ = cx;
            },
        ));
        let uid = view.item_uid.clone();
        if can_start {
            row = row.child(act_btn(
                cx,
                t(lang, Key::UpgradeBtn),
                pal.warn,
                0xffffff,
                move |this, cx| {
                    let cmd = UpgradeStart {
                        item_uid: uid.clone(),
                        preview_id: preview_id.clone(),
                    };
                    this.send_cmd(&cmd, Act::Upgrade);
                    let _ = cx;
                },
            ));
        } else {
            row = row.child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(pal.border))
                    .text_color(rgb(pal.muted))
                    .child(if pending {
                        t(lang, Key::Loading)
                    } else {
                        t(lang, Key::UpgradeBtn)
                    }),
            );
        }
        col = col.child(row);
        col
    }

    /// Инкрустация посоха (ФР-09).
    pub(crate) fn socket_view(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        staff_uid: String,
    ) -> Div {
        let lang = self.lang;
        let Some(snap) = self.store.snapshot.clone() else {
            return div().size_full();
        };
        let Some(staff) = snap
            .char
            .inventory
            .iter()
            .find(|i| i.uid == staff_uid)
            .cloned()
        else {
            self.store.close_socketing();
            return div().size_full();
        };
        let runes: Vec<ItemSnapshot> = snap
            .char
            .inventory
            .iter()
            .filter(|i| i.item_type == "rune")
            .cloned()
            .collect();

        let mut col = div().flex().flex_col().gap_1();
        for socket in staff.sockets.iter().flatten() {
            let mut row = div().flex().flex_row().gap_2().items_center();
            row = row.child(
                div()
                    .text_color(rgb(pal.muted))
                    .child(format!("◈ {}", socket.index)),
            );
            match &socket.rune {
                Some(rune) => {
                    let index = socket.index;
                    row = row.child(
                        div().child(format!("{} ({}/{})", rune.name, rune.charges, rune.max)),
                    );
                    let suid = staff.uid.clone();
                    row = row.child(act_btn(
                        cx,
                        t(lang, Key::ExtractRune),
                        pal.panel,
                        pal.text,
                        move |this, cx| {
                            let cmd = SocketExtract {
                                staff_uid: suid.clone(),
                                socket_index: index,
                            };
                            this.send_cmd(&cmd, Act::Socket);
                            let _ = cx;
                        },
                    ));
                }
                None => {
                    if runes.is_empty() {
                        row = row.child(
                            div()
                                .text_color(rgb(pal.muted))
                                .child(t(lang, Key::NoRunes)),
                        );
                    }
                    for rune in &runes {
                        let index = socket.index;
                        let suid = staff.uid.clone();
                        let ruid = rune.uid.clone();
                        let label = format!("{}: {}", t(lang, Key::InsertRune), rune.name);
                        row =
                            row.child(act_btn(cx, label, pal.panel, pal.text, move |this, cx| {
                                let cmd = SocketInsert {
                                    staff_uid: suid.clone(),
                                    socket_index: index,
                                    rune_uid: ruid.clone(),
                                };
                                this.send_cmd(&cmd, Act::Socket);
                                let _ = cx;
                            }));
                    }
                }
            }
            col = col.child(row);
        }

        if let Some(err) = &self.store.sheet_error.clone() {
            col = col.child(div().text_color(rgb(pal.danger)).child(err.clone()));
        }
        col = col.child(act_btn(
            cx,
            t(lang, Key::Exit),
            pal.panel,
            pal.text,
            |this, cx| {
                this.store.close_socketing();
                let _ = cx;
            },
        ));
        col
    }
}

// ── вспомогательные ────────────────────────────────────────────────────────

/// Сводка целей квеста: «Волков убито 4/5» или текст цели.
fn goals_summary(q: &ld2_protocol::types::QuestState) -> String {
    q.goals
        .iter()
        .map(|g| match (g.cur, g.need) {
            (Some(cur), Some(need)) => format!("{} {cur}/{need}", g.text),
            _ => g.text.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Название профессии: l10n-ключ по id, фолбэк — сам id.
fn prof_label(lang: Lang, id: &str) -> String {
    let key = match id {
        "blacksmith" => Some(Key::ProfBlacksmith),
        "alchemist" => Some(Key::ProfAlchemist),
        "scribe" => Some(Key::ProfScribe),
        "woodcarver" => Some(Key::ProfWoodcarver),
        "tailor" => Some(Key::ProfTailor),
        "cook" => Some(Key::ProfCook),
        "herbalist" => Some(Key::ProfHerbalist),
        "hunter" => Some(Key::ProfHunter),
        _ => None,
    };
    key.map(|k| t(lang, k).to_string())
        .unwrap_or_else(|| id.to_string())
}

/// M15/F25: строка «подпись … значение» в панели опыта окна персонажа.
fn xp_kv_row(pal: &Palette, label: &str, value: &str) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .text_size(px(12.0))
        .child(div().text_color(rgb(pal.muted)).child(label.to_string()))
        .child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(value.to_string()),
        )
}

/// Бейджи плитки: +N, сокеты ◈○, количество ×N (паритет egui).
fn badges(item: &ItemSnapshot) -> String {
    let mut parts: Vec<String> = Vec::new();
    if item.upgrade > 0 {
        parts.push(format!("+{}", item.upgrade));
    }
    if let Some(sockets) = &item.sockets {
        let filled = sockets.iter().filter(|s| s.rune.is_some()).count();
        let empty = sockets.len() - filled;
        parts.push(format!("{}{}", "◈".repeat(filled), "○".repeat(empty)));
    }
    if let Some(qty) = item.qty.filter(|q| *q > 1) {
        parts.push(format!("×{qty}"));
    }
    parts.join(" ")
}

/// Редкость → цвет (палитра §7.4).
/// MenuItem с действием над Root (запись через entity: замыкания `on_click`
/// должны быть `'static`, а `&mut Root` живёт только в цикле сборки).
fn menu_item(
    cx: &mut Context<'_, Root>,
    label: impl Into<SharedString>,
    body: impl Fn(&mut Root, &mut Context<'_, Root>) + 'static,
) -> MenuItem {
    let this = cx.entity();
    MenuItem::new(label).on_click(move |_window, acx| {
        this.update(acx, |root, rcx| {
            body(root, rcx);
            rcx.notify();
        });
    })
}

/// Цвет редкости предмета (палитра §7.4).
fn rarity_color(rarity: &str, pal: &Palette) -> u32 {
    match rarity {
        "common" => pal.muted,
        "uncommon" => 0x98_c3_79,
        "rare" => 0x61_af_ef,
        "epic" => 0xc6_78_dd,
        "legendary" => 0xe5_c0_7b,
        _ => pal.text,
    }
}

/// Слот экипировки для типа предмета (гейт клиента; сервер — истина, §7).
fn slot_for(item_type: &str) -> Option<&'static str> {
    match item_type {
        "staff" => Some("staff"),
        "weapon" => Some("main_hand"),
        "armor" => Some("body"),
        _ => None,
    }
}

/// Цвет severity (палитра §7.4).
fn severity_rgb(severity: Severity) -> u32 {
    match severity {
        Severity::High => 0xe0_6c_75,
        Severity::Medium => 0xd1_9a_66,
        Severity::Low => 0x98_c3_79,
        Severity::Unknown => 0xab_b2_bf,
    }
}

/// Полоса-прогресс (паритет egui ProgressBar).
pub(crate) fn bar(frac: f32, label: String, fill: u32, pal: &Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(140.0))
                .h(px(10.0))
                .rounded_md()
                .bg(rgb(pal.border))
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(frac.clamp(0.0, 1.0)))
                        .rounded_md()
                        .bg(rgb(fill)),
                ),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(pal.muted))
                .child(label),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M15/F28: группы — в порядке GDD §3, иды уникальны.
    #[test]
    fn skill_groups_match_gdd_order() {
        let ids: Vec<&str> = SKILL_GROUPS.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            ["weapon", "defense", "magic", "stealth", "practice", "social"],
            "порядок групп как в skills.md §3 (A–F)"
        );
        let unique: std::collections::HashSet<_> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len(), "иды групп уникальны");
    }

    /// M15/F25: состав и порядок вкладок окна — по спеке пользователя
    /// (Параметры | Навыки | Профессии | Репутация + нетронутые).
    #[test]
    fn character_window_tabs_cover_spec() {
        let tabs: Vec<&'static str> = SheetTab::all().iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            tabs,
            [
                "params",
                "skills",
                "professions",
                "reputation",
                "quests",
                "guild",
                "inventory",
            ],
            "порядок вкладок правой колонки"
        );
        for (tab, _) in SheetTab::all() {
            assert_eq!(SheetTab::parse(tab.as_str()), tab, "round-trip");
        }
        // Старые значения удалённых вкладок (правило дубликатов)
        // из сохранённого конфига маппятся на «Параметры».
        assert_eq!(SheetTab::parse("attrs"), SheetTab::Params);
        assert_eq!(SheetTab::parse("statuses"), SheetTab::Params);
    }
}

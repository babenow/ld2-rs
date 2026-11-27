//! Игровой экран gpui (M10.4) — порт `ui/world.rs` egui-клиента:
//! левая колонка (цель/ресурсы/история), центр (локация, сущности с
//! контекстным меню, hotbar, журнал), модалки смерти/осмотра/лута.
//!
//! Правая панель-лист (sheet, 7 вкладок) и соц-окна (пати/сделка) — M10.4b/M10.5.
//! Отличия от egui: модалки заменяют контент (шапка остаётся), контекстное
//! меню — инлайн под строкой (вместо попапа по курсору), тултипы не используются.

use ld2_core::domain;

use ely::overlays::Dialog;
use gpui::{
    div, prelude::*, px, rgb, Animation, AnimationExt, AnyElement, Context, Div, EmptyView,
    MouseButton, SharedString, Stateful, Window,
};
use ld2_core::l10n::{format_duration, t, tf, Key};
use ld2_core::store::{
    crime_kind_title, filter_active_buffs, filter_active_statuses, local_epoch, local_epoch_ms,
    Act, AppStore, LogKind, NpcDialogView, NpcProfession, Screen,
};
use ld2_protocol::evt::StateSnapshot;
use ld2_protocol::types::{Entity, EntityKind, ItemSnapshot, LocationSnapshot, Severity};

use crate::fa::fa_icon;
use crate::palette::Palette;
use ely::buttons::IconButton;
use ely::forms::Choice;
use ely::layout::ScrollArea;
use ely::menus::{ContextMenu, Menu, MenuItem};
use ely::navigation::Tabs;
use ely::primitives::{Divider, IconName, Tooltip, TooltipTrigger};

use super::quests::goals_summary;
use super::sheet::rarity_label;
use super::social::show_party;
use super::{
    act_btn, act_btn_hover, btn, hex_to_u32, scrollable_tab, status_badge, Click, MapPan,
    PopupKind, Root,
};

const POWER_STRIKE_RANK: u32 = 5;
const MS_PER_SECOND: i64 = 1000;
const ROUND_UP_MS: i64 = 999;

mod dialog;
mod entities;
mod hotbar;
mod map;
mod shop;

mod hud;

/// M14/F4: «Мощный удар» открывается владением мечом 5 (skills.md §4;
/// серверный гейт — конфиг `skills.active.power_strike` → `E_REQUIREMENT_ATTR`).
/// Клиентское зеркало гейта: кнопка и хоткей показываются/срабатывают
/// только когда навык открыт.
pub(crate) fn power_strike_open(skills: &[ld2_protocol::types::SkillSnapshot]) -> bool {
    skills
        .iter()
        .find(|s| s.id == domain::SKILL_SWORD)
        .is_some_and(|s| s.rank >= POWER_STRIKE_RANK)
}

/// M17/F30b: uid надетого посоха — слот `staff` удалён (посох теперь
/// двуручное оружие правой руки), ищем по типу среди надетого.
/// M24: надетая экипировка с вставленными рунами — кнопки каста на
/// панели и хоткей «руна». Раньше брался только посох (`item_type ==
/// "staff"`): руны в мече/броне (ADR-012) кнопки не получали —
/// активная руна «не активировалась» (указание заказчика).
fn equipped_rune_items(snap: &StateSnapshot) -> Vec<&ItemSnapshot> {
    let equipped: std::collections::HashSet<&str> = snap
        .char
        .equip
        .values()
        .flatten()
        .map(String::as_str)
        .collect();
    snap.char
        .inventory
        .iter()
        .filter(|i| equipped.contains(i.uid.as_str()))
        .filter(|i| {
            i.sockets
                .iter()
                .flatten()
                // M24: самоцветы живут в том же поле `rune`, но
                // заклинания не имеют — на панель каста не идут.
                .any(|s| s.rune.as_ref().is_some_and(|r| !r.spell.is_empty()))
        })
        .collect()
}

impl Root {
    /// M14/F4: первая доступная руна посоха (хоткей «руна») — заряды и
    /// мана в порядке, как у первой включённой кнопки рун на панели.
    pub(crate) fn first_rune_click(&self) -> Option<Click> {
        let snap = self.store.snapshot.as_ref()?;
        let rune_cost = self
            .store
            .session
            .as_ref()
            .map(|s| s.magic.mana.rune_cost)
            .unwrap_or(0);
        for staff in equipped_rune_items(snap) {
            if let Some(socket) = staff.sockets.iter().flatten().find(|socket| {
                socket.rune.as_ref().is_some_and(|r| {
                    r.charges > 0
                        // M24: самоцвет — не руна, каст не из него.
                        && !r.spell.is_empty()
                        && (rune_cost <= 0 || snap.char.mp.cur >= rune_cost)
                        // M24: класс руны — каст только своему классу.
                        && r.class
                            .as_deref()
                            .is_none_or(|c| snap.char.profile.as_deref() == Some(c))
                })
            }) {
                return Some(Click::RuneCast {
                    staff_uid: staff.uid.clone(),
                    socket_index: socket.index,
                });
            }
        }
        None
    }

    /// M14/F4: первый расходник боя (свиток/зелье) — хоткей «6».
    pub(crate) fn first_consumable_uid(&self) -> Option<String> {
        self.store
            .snapshot
            .as_ref()?
            .char
            .inventory
            .iter()
            .find(|i| {
                matches!(
                    i.item_type.as_str(),
                    domain::ITEM_SCROLL | domain::ITEM_POTION
                )
            })
            .map(|i| i.uid.clone())
    }

    pub(crate) fn game_screen(&mut self, cx: &mut Context<'_, Self>, pal: &Palette) -> Div {
        let Some(snap) = self.store.snapshot.clone() else {
            return div()
                .flex()
                .items_center()
                .justify_center()
                .size_full()
                .child(t(self.lang, Key::Loading));
        };

        let row = div()
            .flex()
            .flex_row()
            .gap_3()
            .flex_grow_1()
            .min_h_0()
            .p(px(crate::ui_tokens::SPACE_12))
            // M14/F18: резиновая вёрстка — min_w_0 позволяет колонкам
            // сжиматься ниже min-content (иначе правая колонка
            // вылезала за рамки окна); min_w(360) у листа остаётся
            // жёстким полом на минимуме окна (960×640).
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    // M25: левая колонка — 20 % ширины экрана
                    // (резиновая; пол 240 — не схлопывается на узких
                    // окнах), центр — гибкий, правая —20 %.
                    .w(gpui::relative(0.2))
                    .min_w(px(240.0))
                    .child(self.left_column(cx, pal, &snap))
                    // Заказчик 2026-11-20: чат — в левой колонке, к низу;
                    // высота 30 % (после разворачивания — 50 %), сворачивание
                    // из шапки чата, открытие — кнопкой в статус-баре.
                    .children((!self.config.chat_collapsed).then(|| {
                        div()
                            .flex()
                            .flex_col()
                            .flex_grow_1()
                            .min_h_0()
                            .child(div().flex_grow_1().min_h_0())
                            .child(self.journal_block(cx, pal))
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .flex_grow_1()
                    .min_w_0()
                    // Не выше остатка экрана: журнальный блок сжимается
                    // до своего min_h, а не выдавливает колонку наружу.
                    .min_h_0()
                    // M14/F2: описание локации — всегда первая строка.
                    .child(self.location_block(pal, &snap.location))
                    // Таргет — под описанием (перенос из левой колонки).
                    .child(self.target_block(cx, pal, &snap))
                    .children(self.notice_line(pal))
                    .child(self.entities_block(cx, pal, &snap.location))
                    .child(self.hotbar(cx, pal, &snap))
                    .child(self.exits_row(cx, pal, &snap.location))
                    .children((self.store.target.is_some()).then(|| {
                        div()
                            .text_size(px(crate::ui_tokens::FONT_SM))
                            .text_color(rgb(pal.muted))
                            .child(t(self.lang, Key::AltNavHint))
                    })),
            )
            // M26: правая колонка —20 %: карта-граф мира (верх,
            // вместо истории посещений — заказчик) и трекер квестов.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .min_w_0()
                    .min_h_0()
                    .w(gpui::relative(0.2))
                    .min_w(px(180.0))
                    .child(self.map_block(pal))
                    .children(self.tracker_block(pal)),
            );
        // Статус-бар (заказчик 2026-11-20): слева — кнопка «Чат» (видна,
        // когда чат свёрнут — разворачивает его на 50 %); справа — статус
        // сервера (переехал из шапки).
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(row)
            .child(self.status_bar(cx, pal))
    }
}

/// Полоса-прогресс (паритет egui ProgressBar).
/// M17/F30f: сегменты строки лога по маркерам `**...**` —
/// (текст, жирный). Пробелы остаются в исходных сегментах.
fn split_bold(text: &str) -> Vec<(&str, bool)> {
    let mut out: Vec<(&str, bool)> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("**") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("**") else {
            break; // незакрытый маркер — хвост как есть
        };
        if start > 0 {
            out.push((&rest[..start], false));
        }
        out.push((&after[..end], true));
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        out.push((rest, false));
    }
    out
}

/// M18: ширина шкалы — общая для здоровья и ресурса (одинаковые
/// прогрессбары в левой колонке).
const BAR_WIDTH: f32 = 140.0;

fn bar(frac: f32, label: String, fill: u32, pal: &Palette) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(BAR_WIDTH))
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
                .text_size(px(crate::ui_tokens::FONT_SM))
                .text_color(rgb(pal.muted))
                .child(label),
        )
}

/// M25: виджет шкалы (заказчик): цифры ПОВЕРХ бара (внутри), заливка
/// — `color` (ХП остаётся зелёным), текст белый с тёмным контуром —
/// контраст и к заливке, и к фону трека. Параметры: цвет, максимум,
/// текущее, надпись, ширина, высота.
fn stat_bar(
    color: u32,
    max: i64,
    cur: i64,
    label: &str,
    width: f32,
    height: f32,
    pal: &Palette,
) -> Div {
    let frac = (cur as f32 / max.max(1) as f32).clamp(0.0, 1.0);
    let text = format!("{label} {cur}/{max}");
    let text_size = (height - 6.0).max(9.0);
    div()
        .relative()
        .w(px(width))
        .h(px(height))
        .child(
            div()
                .absolute()
                .size_full()
                .rounded_md()
                .bg(rgb(pal.border))
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(frac))
                        .rounded_md()
                        .bg(rgb(color)),
                ),
        )
        // Тёмная полупрозрачная плашка + белый жирный — читается
        // и на заливке, и на треке (выбор заказчика).
        .child(
            div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .px_1()
                        .rounded_md()
                        .bg(gpui::rgba(0x00_00_00_99))
                        .text_size(px(text_size))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(rgb(0xff_ff_ff))
                        .child(text),
                ),
        )
}

/// M14/F4: почему кнопка панели недоступна: требование (цель/мана/
/// заряды — текст в тултипе) или кулдаун (таймер на кнопке + тултип).
enum OffReason {
    Req(String),
    Cd(i64),
}

/// M14/F4: недоступная кнопка панели — причина в тултипе (тултипы
/// используются и для включённых кнопок — egui-паритет M11.4).
fn action_disabled(id: &str, label: String, reason: String, pal: &Palette) -> Stateful<Div> {
    div()
        .id(id.to_string())
        .tooltip(Tooltip::text(reason))
        .px_3()
        .py_1()
        .rounded_md()
        .bg(rgb(pal.border))
        .text_color(rgb(pal.muted))
        .child(label)
}

/// M14/F4: текст причины «кулдаун» — остаток округляется вверх
/// (100 мс до конца → «1 с»), чтобы значение не мигало в 0.
fn cooldown_reason(lang: ld2_core::l10n::Lang, left_ms: i64) -> String {
    let secs = ((left_ms.max(0) + ROUND_UP_MS) / MS_PER_SECOND).max(1);
    tf(lang, Key::HotbarCooldown, &[("{s}", &secs.to_string())])
}

/// M14/F4: кнопка на кулдауне (собственный/глобальный): таймер остатка
/// прямо на кнопке (перерисовка тиком 50 мс) + причина в тултипе.
fn cd_disabled(
    id: &str,
    label: String,
    left_ms: i64,
    lang: ld2_core::l10n::Lang,
    pal: &Palette,
) -> Stateful<Div> {
    let secs = ((left_ms.max(0) + ROUND_UP_MS) / MS_PER_SECOND).max(1);
    let short = tf(lang, Key::HotbarCdShort, &[("{s}", &secs.to_string())]);
    action_disabled(
        id,
        format!("{label} · {short}"),
        cooldown_reason(lang, left_ms),
        pal,
    )
}

/// M17/F30g: кнопка в предметном кулдауне — таймер на кнопке и
/// анимированный прогрессбар под ней (остаток от полной длительности;
/// анимация — тик 50 мс через cd_active в root).
fn cd_bar_button(
    id: &str,
    label: String,
    left_ms: i64,
    total_ms: i64,
    lang: ld2_core::l10n::Lang,
    pal: &Palette,
) -> Div {
    let secs = ((left_ms.max(0) + ROUND_UP_MS) / MS_PER_SECOND).max(1);
    let short = tf(lang, Key::HotbarCdShort, &[("{s}", &secs.to_string())]);
    let frac = (left_ms as f32 / total_ms.max(1) as f32).clamp(0.0, 1.0);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(action_disabled(
            id,
            format!("{label} · {short}"),
            cooldown_reason(lang, left_ms),
            pal,
        ))
        .child(
            div().h(px(3.0)).rounded_md().bg(rgb(pal.border)).child(
                div()
                    .h_full()
                    .w(gpui::relative(frac))
                    .rounded_md()
                    .bg(rgb(pal.accent)),
            ),
        )
}

/// Цвет severity (палитра §7.4, локальная копия — egui-версия возвращает Color32).
fn severity_rgb(severity: Severity) -> u32 {
    match severity {
        Severity::High => 0xe0_6c_75,
        Severity::Medium => 0xd1_9a_66,
        Severity::Low => 0x98_c3_79,
        Severity::Unknown => 0xab_b2_bf,
    }
}

/// Цвет строки журнала (палитра §7.4; светлая тема — затемнение светлых,
/// паритет M9.2 egui-клиента).
fn log_color(kind: LogKind, dark: bool) -> u32 {
    use LogKind::*;
    if !dark {
        return match kind {
            System => 0x5a_5e_66,
            Location => 0x4a_4e_58,
            Chat => 0x2a_2a_2e,
            // Каналы чата (заказчик): каждая — свой цвет.
            ChatGlobal => 0x2b_6c_b0,
            ChatParty => 0x2e_7d32,
            ChatGuild => 0x8e_44_ad,
            ChatWhisper => 0xc2_18_5b,
            ChatSystem => 0xb2_6a_00,
            Xp => 0x2e_7d_91,
            Loot => 0x8a_6d_1f,
            Crime => 0xa5_5a_22,
            _ => log_color(kind, true),
        };
    }
    match kind {
        System => 0x7f_84_8e,
        Location => 0xab_b2_bf,
        CombatOut => 0xe0_6c_75,
        CombatIn => 0x98_c3_79,
        Magic => 0x61_af_ef,
        Loot => 0xe5_c0_7b,
        Crime => 0xd1_9a_66,
        Quest => 0xc6_78_dd,
        Xp => 0x56_b6_c2,
        Chat => 0xeb_eb_eb,
        ChatGlobal => 0x61_af_ef,
        ChatParty => 0x98_c3_79,
        ChatGuild => 0xc6_78_dd,
        ChatWhisper => 0xe8_7a_93,
        ChatSystem => 0xe5_c0_7b,
    }
}

/// Подсказка строки сущности: HP и теги (имя — уже в строке).
fn entity_tooltip(entity: &Entity) -> String {
    let hp = entity
        .hp
        .map(|hp| format!(" · {}/{}", hp.cur, hp.max))
        .unwrap_or_default();
    let tags = entity
        .tags
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    // M32: защита моба (сервер кладёт статы в `extra`): броня, уклон,
    // парирование, магрезист, точность — видно в подсказке списка.
    let stat = |key: &str| entity.extra.get(key).and_then(|v| v.as_i64());
    let combat = if entity.kind == ld2_protocol::types::EntityKind::Monster {
        match (
            stat("armor"),
            stat("dodge"),
            stat("parry"),
            stat("resist"),
            stat("accuracy"),
        ) {
            (Some(a), Some(d), Some(p), Some(r), Some(acc)) => format!(
                "\nБроня {a} · уклон {d}% · парирование {p}% · сопротивление {r}% · точность {acc}"
            ),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    if tags.is_empty() {
        format!("{}{hp}{combat}", entity.name)
    } else {
        format!("{}{hp} · {tags}{combat}", entity.name)
    }
}

/// M26: узел карты в пиксельных координатах (для рендера слоя).
pub(crate) struct MapNodeView {
    pub id: String,
    pub x: f32,
    pub y: f32,
    pub name: String,
    pub text: String,
    pub current: bool,
    pub visited: bool,
}

/// M26: плечо карты в пиксельных координатах.
pub(crate) struct MapEdgeView {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    /// Глиф направления (↑↓←→…) — подпись плеча (заказчик).
    pub glyph: &'static str,
    /// Имя выхода — тултип плеча (заказчик).
    pub label: String,
}

/// M26: общий слой карты (F26c колонка / F26d окно): линии-плечи —
/// canvas + `paint_path` (тонкие четырёхугольники — любой угол,
/// включая диагонали компаса); узлы — точки с тултипом «название +
/// описание»; в середине плеча — глиф направления с тултипом «имя
/// выхода». Только наведение (заказчик: клик — не действие).
/// `shell` — контейнер с размером (flex_grow/h_full): сюда же
/// вешаются жесты зума/панорамы (F26d).
pub(crate) fn map_graph_layer(
    pal: &Palette,
    nodes: Vec<MapNodeView>,
    edges: Vec<MapEdgeView>,
    shell: Stateful<Div>,
    dot: f32,
    hit: f32,
) -> Stateful<Div> {
    // Линии-плечи: от центра к центру (клетка + половина хит-бокса),
    // тонкий четырёхугольник ~3 px (заказчик: «соединения — линиями»).
    let half = hit / 2.0;
    let lines: Vec<(gpui::Point<gpui::Pixels>, gpui::Point<gpui::Pixels>)> = edges
        .iter()
        .map(|e| {
            (
                gpui::point(px(e.x1 + half), px(e.y1 + half)),
                gpui::point(px(e.x2 + half), px(e.y2 + half)),
            )
        })
        .collect();
    let line_color: gpui::Hsla = gpui::rgba((pal.text << 8) | 0xcc).into();
    // `paint_path` рисует в оконных координатах — прибавляем origin
    // canvas'а (без этого линии уезжали в левый верхний угол окна и
    // срезались клипом блока — заказчик «нет рёбер»).
    let canvas = gpui::canvas(
        move |_bounds, _window, _cx| {},
        move |bounds, _state, window, _cx| {
            let ox = f32::from(bounds.origin.x);
            let oy = f32::from(bounds.origin.y);
            for (a, b) in &lines {
                let (ax, ay) = (f32::from(a.x) + ox, f32::from(a.y) + oy);
                let (bx, by) = (f32::from(b.x) + ox, f32::from(b.y) + oy);
                let (dx, dy) = (bx - ax, by - ay);
                let len = (dx * dx + dy * dy).sqrt();
                if len < 1.0 {
                    continue;
                }
                let (nx, ny) = (-dy / len * 1.5, dx / len * 1.5);
                let mut path = gpui::Path::new(gpui::point(px(ax + nx), px(ay + ny)));
                path.line_to(gpui::point(px(bx + nx), px(by + ny)));
                path.line_to(gpui::point(px(bx - nx), px(by - ny)));
                path.line_to(gpui::point(px(ax - nx), px(ay - ny)));
                window.paint_path(path, line_color);
            }
        },
    );

    // Глифы направлений — в середине плеча (тултип — имя выхода);
    // id по индексу — стабильный порядок кадров.
    let glyph_els: Vec<AnyElement> = edges
        .iter()
        .enumerate()
        .map(|(ix, e)| {
            let tip = e.label.clone();
            // Середина линии (центры клеток = top-left + half).
            let mx = (e.x1 + e.x2) / 2.0 + half;
            let my = (e.y1 + e.y2) / 2.0 + half;
            div()
                .id(format!("ld2-map-edge-{ix}"))
                .absolute()
                .left(px(mx - 10.0))
                .top(px(my - 10.0))
                .w(px(20.0))
                .text_size(px(crate::ui_tokens::FONT_LG))
                .text_color(rgb(pal.text))
                .text_center()
                .tooltip(Tooltip::text(tip))
                .child(e.glyph)
                .into_any_element()
        })
        .collect();

    // Узлы: большие кружки (заказчик); текущая — крупнее акцентная
    // с пульсирующим ореолом («где я на карте») и плашкой имени;
    // посещённые — ярче; тултип «название + описание».
    let node_els = map_graph_nodes(pal, nodes, dot, hit);

    shell
        .overflow_hidden()
        .relative()
        // Линии под узлами; абсолютные дети позиционируются отсюда
        // же — координаты совпадают с canvas.
        .child(canvas.w_full().h_full())
        .children(node_els)
        .children(glyph_els)
}

/// M26: узлы карты — точки, тултипы и плашка текущей локации
/// (RF3m: вынесено из `map_graph_layer`).
fn map_graph_nodes(pal: &Palette, nodes: Vec<MapNodeView>, dot: f32, hit: f32) -> Vec<AnyElement> {
    let node_els: Vec<AnyElement> = nodes
        .iter()
        .flat_map(|n| {
            let (dot_border, dot_bg) = if n.current {
                (pal.accent, pal.accent)
            } else if n.visited {
                (pal.text, pal.text)
            } else {
                (pal.muted, pal.panel)
            };
            // Кружок: текущая — крупнее (заказчик: «большие кружки»,
            // «где я на карте»); рамка 2px у всех.
            let size = if n.current { dot + 8.0 } else { dot };
            let circle = div()
                .size(px(size))
                .rounded(px(size / 2.0))
                .border(px(2.0))
                .border_color(rgb(dot_border))
                .bg(rgb(dot_bg));
            // Пульсирующий ореол текущей локации — «где я» видно
            // сразу (заказчик: не могу понять, где я на карте).
            let accent = pal.accent;
            let circle = if n.current {
                circle
                    .with_animation(
                        SharedString::from(format!("map-halo-{}", n.id)),
                        Animation::new(std::time::Duration::from_millis(1600)).repeat(),
                        move |el, t| {
                            let alpha = (0.35 + 0.30 * (2.0 * std::f32::consts::PI * t).sin())
                                .clamp(0.10, 0.70);
                            el.shadow(vec![gpui::BoxShadow {
                                color: gpui::rgba((accent << 8) | ((alpha * 255.0) as u32)).into(),
                                offset: gpui::point(px(0.0), px(0.0)),
                                blur_radius: px(14.0),
                                spread_radius: px(4.0),
                                inset: false,
                            }])
                        },
                    )
                    .into_any_element()
            } else {
                circle.into_any_element()
            };
            let tip_name = n.name.clone();
            let tip_text = n.text.clone();
            let (tip_panel, tip_border, tip_fg) = (pal.panel, pal.border, pal.text);
            let hit_box = div()
                .id(format!("ld2-map-node-{}", n.id))
                .size(px(hit))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .hover(move |st| st.bg(rgb(pal.selection)))
                .child(circle);
            let node_id = n.id.clone();
            let tip_el =
                TooltipTrigger::new(format!("ld2-map-tip-{node_id}"), hit_box, move |_, _| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .mx(px(-8.0))
                        .my(px(-4.0))
                        .p(px(crate::ui_tokens::SPACE_10))
                        .w(px(300.0))
                        .rounded_md()
                        .bg(rgb(tip_panel))
                        .border_1()
                        .border_color(rgb(tip_border))
                        .text_color(rgb(tip_fg))
                        .text_size(px(crate::ui_tokens::FONT_MD))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(tip_name.clone()),
                        )
                        .child(div().child(tip_text.clone()))
                        .into_any_element()
                });
            let mut els = vec![div()
                .absolute()
                .left(px(n.x))
                .top(px(n.y))
                .size(px(hit))
                .child(tip_el)
                .into_any_element()];
            if n.current {
                // Плашка имени текущей — всегда на виду, поверх
                // линий (фон плашки).
                els.push(
                    div()
                        .absolute()
                        .left(px(n.x + hit + 6.0))
                        .top(px(n.y + (hit - 20.0) / 2.0))
                        .px_1()
                        .py(px(1.0))
                        .rounded(px(4.0))
                        .bg(rgb(tip_panel))
                        .border_1()
                        .border_color(rgb(pal.accent))
                        .text_size(px(crate::ui_tokens::FONT_MD))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(rgb(pal.accent))
                        .child(n.name.clone())
                        .into_any_element(),
                );
            }
            els
        })
        .collect();
    node_els
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M17/F30f: маркеры **...** → сегменты; незакрытый не ломает строку.
    #[test]
    fn split_bold_marks_segments() {
        assert_eq!(
            split_bold("Bbnf: **Farta Lussen**, вы бьёте Волк струёй пламени **10**"),
            vec![
                ("Bbnf: ", false),
                ("Farta Lussen", true),
                (", вы бьёте Волк струёй пламени ", false),
                ("10", true),
            ]
        );
        assert_eq!(
            split_bold("обычная строка"),
            vec![("обычная строка", false)]
        );
        let open = split_bold("хвост **без пары");
        assert_eq!(
            open,
            vec![("хвост **без пары", false)],
            "незакрытый — как есть"
        );
    }
}

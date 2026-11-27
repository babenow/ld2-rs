//! Окно персонажа: popup (RF3l).

use super::*;

impl Root {
    /// M16/F29b: оболочка всплывающего окна (механика M15/F25b–g):
    /// слой с карточкой — drag за шапку, resize уголком; `body` —
    /// содержимое под шапкой. Скрим общий для всех окон (могут быть
    /// открыты одновременно) — `Root::popup_scrim`; геометрия и
    /// флаги жестов — `PopupState` вида `kind`.
    // Оболочка окна: контекст (окно/cx/палитра) + вид + заголовок +
    // тело + подвал — набор неизменен, кроме растущих частей.
    /// Оболочка без правой группы в шапке (большинство окон → None).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn popup_window(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        kind: PopupKind,
        title: impl Into<SharedString>,
        leading: Option<Div>,
        body: impl IntoElement,
        footer: Option<Div>,
    ) -> AnyElement {
        self.popup_window_full(
            window,
            cx,
            pal,
            kind,
            title,
            leading.map(|d| d.into_any_element()),
            body,
            footer,
            None,
        )
    }

    /// То же + `trailing` — группа справа, слева от крестика закрытия
    /// (M24: тумблер «режим бога» в админ-панели).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn popup_window_full(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        kind: PopupKind,
        title: impl Into<SharedString>,
        // M18/F41: кнопки действий слева в шапке (окно диалога NPC —
        // магазин/«К диалогу»); у остальных окон — None.
        leading: Option<AnyElement>,
        body: impl IntoElement,
        // M16/F29g: нижняя панель под телом (по аналогии с шапкой).
        footer: Option<Div>,
        // M24: правая группа в шапке (слева от крестика).
        trailing: Option<AnyElement>,
    ) -> AnyElement {
        let title = title.into();
        let (w, h) = self.popup(kind).size;
        let delta = self.popup(kind).delta;
        let id = kind.id();

        // M15/F25c: шапка — drag-регион (захват указателя gpui:
        // события идут, даже когда курсор ушёл за карточку).
        let top = self.popup_top_bar(cx, pal, id, kind, title, leading, trailing);
        let grip_this = cx.entity();
        let resize_grip = div()
            .id(format!("{id}-resize"))
            .absolute()
            .right_0()
            .bottom_0()
            .size(px(18.0))
            .cursor(CursorStyle::ResizeUpLeftDownRight)
            // M15/F25e: жест начат на уголке — scrim не закроет.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |root, _, _, _| {
                    root.popup_scrim_down = false;
                }),
            )
            .on_drag(PopupResize, move |_value, _offset, _window, acx| {
                grip_this.update(acx, |root, cx| {
                    // M16/F29b: жест принадлежит этому окну; новый
                    // захват — новый якорь (иначе скачок по остатку).
                    root.drag_owner = Some(kind);
                    root.popup_mut(kind).resize_anchor = None;
                    cx.notify();
                });
                acx.new(|_| EmptyView)
            })
            .on_drag_move(cx.listener(
                move |root, event: &DragMoveEvent<PopupResize>, window, cx| {
                    if root.drag_owner != Some(kind) {
                        return;
                    }
                    let pos = (
                        f32::from(event.event.position.x),
                        f32::from(event.event.position.y),
                    );
                    let st = root.popup_mut(kind);
                    let (cur_w, cur_h) = st.size;
                    // Якорь — первый move (позицию захвата в конструктор
                    // on_drag не передаём); кадр движения теряем.
                    let Some((ax, ay, w0, h0, dx0, dy0)) = st.resize_anchor else {
                        st.resize_anchor =
                            Some((pos.0, pos.1, cur_w, cur_h, st.delta.0, st.delta.1));
                        return;
                    };
                    let viewport = window.viewport_size();
                    let (nw, nh) = clamp_char_size(
                        w0 + (pos.0 - ax),
                        h0 + (pos.1 - ay),
                        f32::from(viewport.width),
                        f32::from(viewport.height),
                    );
                    // Якорим левый верх угол: компенсируем центрирование
                    // Backdrop (resize от центра сдвигал бы оба края
                    // на Δ/2).
                    st.delta.0 = dx0 + (nw - w0) / 2.0;
                    st.delta.1 = dy0 + (nh - h0) / 2.0;
                    st.size = (nw, nh);
                    cx.notify();
                },
            ));

        // M15/F25d: absolute-позиция от слоя (F14-подход): центр
        // вьюпорта + смещение — марджины центрирования давали лишь
        // половину сдвига («резиновый» drag).
        let viewport = window.viewport_size();
        let (left, top_xy) = char_card_origin(
            f32::from(viewport.width),
            f32::from(viewport.height),
            w,
            h,
            delta,
        );
        let card = div()
            .id(format!("{id}-card"))
            // M15/F25g: hitbox карточки перекрывает scrim под ней —
            // клик в ЛЮБУЮ точку окна попадает в окно, а не в скрим
            // (иначе «пустые» места карточки без interactivity уходили
            // в scrim и закрывали окно «будто по пустоте»).
            .occlude()
            // M15/F25g: любой жест, начавшийся внутри карточки,
            // снимает scrim-флаг — закрыть может только полный клик,
            // целиком прошедший по scrim.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |root, _, _, _| {
                    root.popup_scrim_down = false;
                }),
            )
            // R-dnd (заказчик): ВСЯ карточка рюкзака — зона приёма drag
            // предметов (иначе отпускание на подложке/полях окна
            // открывало подтверждение выброса); маркер ведёт КАРТОЧКА,
            // список не трогает — иначе clear-ы спорили по порядку.
            // R-dnd (заказчик): отпускание В ЛЮБОЙ точке окна рюкзака —
            // приём (раньше поля/подложка окна открывали подтверждение
            // выброса). on_drop — hover hit-test, работает для drag из
            // любого места.
            .on_drop::<ItemDrag>({
                let card_this = cx.entity();
                move |drag: &ItemDrag, _, cx| {
                    if kind != PopupKind::Backpack {
                        return;
                    }
                    card_this.update(cx, |root, cx| {
                        if let DragSource::Equip(slot) = &drag.from {
                            tracing::info!("dnd: drop в окно рюкзака → снять в рюкзак");
                            let cmd = ItemUnequip { slot: slot.clone() };
                            let _ = root.send_or_notice(&cmd, SendSlot::Notice);
                        } else {
                            // Предмет из рюкзака в своё же окно — без
                            // действия (не «выбросить»).
                            tracing::info!("dnd: drop в окно рюкзака → без действия");
                        }
                        root.drag_item = None;
                        root.bp_hold = None;
                        cx.notify();
                    });
                }
            })
            .absolute()
            .left(px(left))
            .top(px(top_xy))
            .flex()
            .flex_col()
            .w(px(w))
            .h(px(h))
            .rounded_md()
            // M16/F29f: шапка прилегает к кромке — карточка срезает
            // её углы своим скруглением.
            .overflow_hidden()
            .border_1()
            .border_color(rgb(pal.border))
            .bg(rgb(pal.panel))
            .child(top)
            // M16/F29f: тело — с внутренними отступами (бывший
            // padding карточки переехал сюда).
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_grow_1()
                    .min_h_0()
                    .p(px(12.0))
                    .child(body),
            )
            // M16/F29g: нижняя панель (по аналогии с верхней) — у рюкзака.
            .children(footer)
            .child(resize_grip);

        // M16/F29b: слой карточки (позиционирование); scrim общий
        // для всех окон — `Root::popup_scrim` лежит под карточками,
        // клик мимо закрывает верхнее открытое окно.
        div()
            .id(format!("{id}-dialog-layer"))
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(card)
            .into_any_element()
    }

    /// M11.2: диалоги листа (ФР-07/09/10): инкрустация/выбор цели.
    /// M18/F53: описание предмета и заточка — попапы (см.
    /// `item_window`/`upgrade_window`), не Ely-модалки.
    pub(crate) fn sheet_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        // M18: окно сокетов — popup (`socket_window`), не модалка.
        // M17/F30a: выбор оружия под инкрустацию руны (пункт меню руны).
        if let Some(rune_uid) = self.store.inscribe.clone() {
            let title = t(self.lang, Key::InscribePick).to_string();
            let body = self.inscribe_view(cx, pal, &rune_uid).into_any_element();
            let close = self.dismiss(cx, |r| r.store.close_inscribe());
            return Some(
                Dialog::new("ld2-inscribe", title, close)
                    .child(body)
                    .into_any_element(),
            );
        }
        None
    }

    /// Отправка команды листа (паритет egui `send`).
    pub(crate) fn send_cmd<C: ld2_protocol::Command>(&mut self, cmd: &C, act: Act) {
        if self.send_or_notice(cmd, SendSlot::Notice) {
            self.store.on_sheet_sent(act)
        }
    }

    /// Шапка попапа: заголовок, leading/trailing-кнопки, перетаскивание
    /// за шапку (RF3l: вынесено из `popup_window_full`).
    #[allow(clippy::too_many_arguments)]
    fn popup_top_bar(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
        id: &str,
        kind: PopupKind,
        title: SharedString,
        leading: Option<AnyElement>,
        trailing: Option<AnyElement>,
    ) -> impl gpui::IntoElement {
        let lang = self.lang;
        let drag_this = cx.entity();
        let top = div()
            .id(format!("{id}-titlebar"))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .cursor_grab()
            .px_3()
            .py_2()
            // M16/F29f: шапка встык к верхней кромке окна (скругление
            // карточки срезает overflow_hidden) и слегка подсвечена.
            .bg(rgb(pal.selection))
            .border_b_1()
            .border_color(rgb(pal.border))
            // M15/F25e: жест начат на шапке — scrim его не закроет.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |root, _, _, _| {
                    root.popup_scrim_down = false;
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    // M18/F41: контекстные кнопки диалога — слева от
                    // заголовка (бывший статус-бар переехал сюда).
                    .children(leading)
                    .child(div().font_weight(gpui::FontWeight::BOLD).child(title)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .children(trailing)
                    .child(
                        IconButton::new(format!("{id}-close"), IconName::X)
                            .tooltip(t(lang, Key::BtnClose))
                            .on_click(cx.listener(move |root, _, _, cx| {
                                root.popup_mut(kind).open = false;
                                // M18/F38: окно диалога закрывается вместе со своим
                                // состоянием (гейт рендера — наличие npc_dialog).
                                if kind == PopupKind::Npc {
                                    tracing::debug!("закрытие диалога NPC: крестик");
                                    root.store.npc_dialog = None;
                                }
                                // M18/F53: окно заточки и описание предмета —
                                // гейт рендера — наличие состояния в store.
                                if kind == PopupKind::Upgrade {
                                    tracing::debug!("закрытие окна заточки: крестик");
                                    root.store.close_upgrade();
                                    root.store.sharpen_stone = None;
                                    root.store.sharpen_list = false;
                                    root.upgrade_progress = None;
                                }
                                if kind == PopupKind::Item {
                                    tracing::debug!("закрытие описания предмета: крестик");
                                    root.store.item_detail = None;
                                }
                                if kind == PopupKind::Socketing {
                                    tracing::info!("закрытие окна сокетов: крестик");
                                    root.store.close_socketing();
                                    root.socket_pick = None;
                                }
                                if kind == PopupKind::Quantity {
                                    tracing::debug!("закрытие окна количества: крестик");
                                    root.quantity_pick = None;
                                }
                                // M23: данные окна подятия не переживают закрытие.
                                if kind == PopupKind::LootPickup {
                                    tracing::debug!("закрытие окна подятия: крестик");
                                    root.close_pickup();
                                }
                                if kind == PopupKind::Dialog {
                                    tracing::debug!("закрытие игрового диалога: крестик");
                                    root.close_game_dialog();
                                }
                                cx.notify();
                            })),
                    ),
            )
            // Захват: запоминаем смещение указателя внутри шапки
            // (конструктор on_drag получает offset от gpui).
            .on_drag(PopupDrag, move |_value, offset, _window, acx| {
                drag_this.update(acx, |root, cx| {
                    // M16/F29b: жест принадлежит этому окну — чужой
                    // on_drag_move на общем payload не должен ехать.
                    root.drag_owner = Some(kind);
                    root.popup_mut(kind).drag_offset =
                        Some((f32::from(offset.x), f32::from(offset.y)));
                    cx.notify();
                });
                acx.new(|_| EmptyView)
            })
            .on_drag_move(cx.listener(
                move |root, event: &DragMoveEvent<PopupDrag>, window, cx| {
                    if root.drag_owner != Some(kind) {
                        return;
                    }
                    let st = root.popup_mut(kind);
                    let Some(offset) = st.drag_offset else {
                        return;
                    };
                    let position = event.event.position;
                    let viewport = window.viewport_size();
                    let (w, h) = st.size;
                    let base_x = ((f32::from(viewport.width) - w) / 2.0).max(0.0);
                    let base_y = ((f32::from(viewport.height) - h) / 2.0).max(0.0);
                    // M16/F29f: шапка встык с кромкой карточки —
                    // её левый верх = указатель − offset (без −12).
                    st.delta.0 = f32::from(position.x) - offset.0 - base_x;
                    st.delta.1 = f32::from(position.y) - offset.1 - base_y;
                    cx.notify();
                },
            ));

        // M15/F25c: уголок изменения размера (правый нижний) — M24:
        // видимого квадратика больше нет (указание заказчика),
        // только курсор диагонального ресайза при наведении.
        top
    }
}

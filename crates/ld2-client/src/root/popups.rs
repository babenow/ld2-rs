//! Всплывающие окна клиента: реестр попапов, общий скрим, тосты,
//! диалоги выхода и подтверждения выброса. Вынесено из `root.rs` (RF3e).

use super::*;

impl Root {
    /// M24: закрытие админ-панели; каталог остаётся в сторе — кэш
    /// на сессию (вариант A), чистится при выходе/смене персонажа.
    pub(super) fn close_admin(&mut self) {
        self.admin_popup.open = false;
        self.admin_catalog_request = false;
        self.admin_target_request = false;
        // Цель и её содержимое не переживают закрытие (сущность могла
        // исчезнуть); каталог остаётся — кэш на сессию.
        self.admin_target = "self".into();
        self.store.admin_target_items = None;
    }

    /// M23: закрытие окна подятия — данные/выбор/запрос чистятся
    /// вместе с флагом (stale-лут не должен пережить закрытие).
    pub(super) fn close_pickup(&mut self) {
        self.pickup_popup.open = false;
        self.store.pickup = None;
        self.pickup_sel = None;
        self.pickup_id = None;
        self.pickup_request = false;
        // Партия «Взять всё» и её подтверждение не переживают окно.
        self.pickup_queue.clear();
        self.pickup_confirm_all = false;
    }

    pub(super) fn open_popup(&mut self, kind: PopupKind) {
        // M18/F33/M24: чистое контекстное меню при каждом открытии —
        // оба меню (см. clear_popup_menu).
        clear_popup_menu(&mut self.bp_menu, &mut self.equip_menu);
        let (vw, vh) = self.viewport;
        let (tw, th) = Self::popup_base_size(kind, self.viewport);
        let (w, h) = if vw > 0.0 && vh > 0.0 {
            clamp_char_size(tw, th, vw, vh)
        } else {
            (tw, th)
        };
        let delta = match kind {
            PopupKind::Character => (0.0, 0.0),
            PopupKind::Backpack => (40.0, 40.0),
            PopupKind::Quests => (-40.0, 40.0),
            PopupKind::Npc => (60.0, -40.0),
            PopupKind::Item => (80.0, -20.0),
            PopupKind::Upgrade => (0.0, -40.0),
            PopupKind::Socketing => (0.0, -40.0),
            PopupKind::Quantity => (0.0, -60.0),
            PopupKind::Dialog => (0.0, -40.0),
            PopupKind::Professions => (0.0, -30.0),
            PopupKind::LootPickup => (40.0, -30.0),
            PopupKind::AdminPanel => (0.0, -20.0),
            PopupKind::Map => (0.0, -30.0),
        };
        let st = self.popup_mut(kind);
        st.open = true;
        st.size = (w, h);
        st.delta = delta;
        st.drag_offset = None;
        st.resize_anchor = None;
        match kind {
            PopupKind::Character => self.sheet_tab = sheet::SheetTab::Params,
            PopupKind::Backpack => {
                self.backpack_tab = backpack::BackpackTab::default();
                self.backpack_bag = None;
            }
            // M18/F36: на «Текущих» при каждом открытии (паритет окон);
            // выбор квеста переживает закрытие — правая колонка
            // корректно откатывается на первой строке вкладки.
            PopupKind::Quests => self.quests_tab = quests::QuestsTab::default(),
            // M18/F38: окно диалога открывает ack, не open_popup.
            PopupKind::Npc => {}
            // M18: описание предмета открывает контекстное меню.
            PopupKind::Item => {}
            // M18/F53: окно заточки открывает контекстное меню.
            PopupKind::Upgrade => {}
            // M18: окно сокетов открывает контекстное меню.
            PopupKind::Socketing => {}
            // M20: окно количества — состояние уже установлено вызывающим.
            PopupKind::Quantity => {}
            // M20: окно диалога открывает `evt/dialog/open`.
            PopupKind::Dialog => {}
            // M21: окно профессий открывает кнопка/хоткей.
            PopupKind::Professions => {}
            // M23: окно подятия — выбор сбрасывает вызывающий.
            PopupKind::LootPickup => {}
            // M24: админ-панель — состояние (поиск/каталог) переживает
            // переоткрытия на сессию.
            PopupKind::AdminPanel => {}
            // M26: карта — при каждом открытии перецентрироваться
            // на текущей локации (pan сбрасывается).
            PopupKind::Map => self.map_pan = None,
        }
    }

    /// M16/F29b: чтение состояния всплывающего окна.
    pub(super) fn popup(&self, kind: PopupKind) -> &PopupState {
        match kind {
            PopupKind::Character => &self.char_popup,
            PopupKind::Backpack => &self.backpack_popup,
            PopupKind::Quests => &self.quests_popup,
            PopupKind::Npc => &self.npc_popup,
            PopupKind::Item => &self.item_popup,
            PopupKind::Upgrade => &self.upgrade_popup,
            PopupKind::Socketing => &self.socket_popup,
            PopupKind::Quantity => &self.quantity_popup,
            PopupKind::Dialog => &self.dialog_popup,
            PopupKind::Professions => &self.professions_popup,
            PopupKind::LootPickup => &self.pickup_popup,
            PopupKind::AdminPanel => &self.admin_popup,
            PopupKind::Map => &self.map_popup,
        }
    }

    /// M16/F29b: состояние всплывающего окна по виду — оболочка
    /// `popup_window` и её жесты работают через этот доступ.
    pub(super) fn popup_mut(&mut self, kind: PopupKind) -> &mut PopupState {
        match kind {
            PopupKind::Character => &mut self.char_popup,
            PopupKind::Backpack => &mut self.backpack_popup,
            PopupKind::Quests => &mut self.quests_popup,
            PopupKind::Npc => &mut self.npc_popup,
            PopupKind::Item => &mut self.item_popup,
            PopupKind::Upgrade => &mut self.upgrade_popup,
            PopupKind::Socketing => &mut self.socket_popup,
            PopupKind::Quantity => &mut self.quantity_popup,
            PopupKind::Dialog => &mut self.dialog_popup,
            PopupKind::Professions => &mut self.professions_popup,
            PopupKind::LootPickup => &mut self.pickup_popup,
            PopupKind::AdminPanel => &mut self.admin_popup,
            PopupKind::Map => &mut self.map_popup,
        }
    }

    /// M16/F29f: открытие окна — базовый размер (персонаж **выше**:
    /// 760×760; рюкзак 760×560), по центру с клампом под вьюпорт
    /// (drag/resize живут до закрытия) и первая вкладка.
    /// M18/F55: базовый размер карточки для `open_popup`.
    ///
    /// Окно описания предмета — **«резиновое»**: ширина считается доли
    /// вьюпорта (28 %, clamp `320..480`) вместо прежних фиксированных
    /// 520 px; высота остаётся константой, докрутит resize-grip.
    /// `clamp` также спасает нулевой viewport до первой отрисовки.
    /// Остальные окна — константы (как были).
    pub(super) fn popup_base_size(kind: PopupKind, viewport: (f32, f32)) -> (f32, f32) {
        let (vw, _) = viewport;
        match kind {
            PopupKind::Character => (760.0, 760.0),
            PopupKind::Backpack => (760.0, 560.0),
            PopupKind::Quests => (800.0, 560.0),
            PopupKind::Npc => (640.0, 560.0),
            PopupKind::Item => ((vw * 0.28).clamp(320.0, 480.0), 420.0),
            PopupKind::Upgrade => (440.0, 360.0),
            PopupKind::Socketing => (440.0, 420.0),
            PopupKind::Quantity => (400.0, 280.0),
            PopupKind::Dialog => (560.0, 460.0),
            PopupKind::Professions => (880.0, 620.0),
            PopupKind::LootPickup => (1040.0, 600.0),
            PopupKind::AdminPanel => (1040.0, 640.0),
            PopupKind::Map => (980.0, 660.0),
        }
    }

    /// M16/F29b: общий scrim группы всплывающих окон — окна могут
    /// быть открыты одновременно (персонаж + рюкзак), поэтому scrim
    /// один на всех и лежит ПОД карточками; клик мимо закрывает
    /// верхнее открытое окно (рюкзак, затем персонаж).
    pub(super) fn popup_scrim(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let npc_open = self.store.npc_dialog.is_some();
        let dialog_open = self.store.dialog.is_some();
        let professions_open = self.professions_popup.open;
        let admin_open = self.admin_popup.open;
        let pickup_open = self.pickup_popup.open;
        let map_open = self.map_popup.open;
        if !self.char_popup.open
            && !self.backpack_popup.open
            && !self.quests_popup.open
            && !npc_open
            && !dialog_open
            && !professions_open
            && !admin_open
            && !pickup_open
            && !map_open
        {
            return None;
        }
        Some(
            div()
                .id("ld2-popup-scrim-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(
                    div()
                        .id("ld2-popup-scrim")
                        .size_full()
                        .bg(rgba(0x99000000))
                        // M15/F25e: запоминаем, что жест начат на scrim.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|root, _, _, _| {
                                root.popup_scrim_down = true;
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|root, _, _, cx| {
                                let close =
                                    scrim_closes_on_up(root.popup_scrim_down, cx.has_active_drag());
                                root.popup_scrim_down = false;
                                if close {
                                    // Верхнее окно: подятие → сокеты → заточка →
                                    // описание → диалог → квесты → рюкзак →
                                    // персонаж (обратный порядок рендера).
                                    if root.pickup_popup.open {
                                        tracing::debug!("закрытие окна подятия: скрим");
                                        root.close_pickup();
                                    } else if root.quantity_pick.is_some() {
                                        root.quantity_pick = None;
                                        root.quantity_popup.open = false;
                                    } else if root.store.socketing.is_some() {
                                        if root.socket_pick.is_some() {
                                            root.socket_pick = None;
                                        } else {
                                            tracing::info!("закрытие окна сокетов: скрим");
                                            root.store.close_socketing();
                                            root.socket_popup.open = false;
                                        }
                                    } else if root.store.sharpen_stone.is_some()
                                        || root.store.upgrade.is_some()
                                    {
                                        root.store.close_upgrade();
                                        root.store.sharpen_stone = None;
                                        root.store.sharpen_list = false;
                                        root.upgrade_progress = None;
                                    } else if root.store.item_detail.is_some() {
                                        root.store.item_detail = None;
                                    } else if root.store.dialog.is_some() {
                                        tracing::debug!("закрытие игрового диалога: скрим");
                                        root.close_game_dialog();
                                    } else if root.professions_popup.open {
                                        tracing::debug!("закрытие окна профессий: скрим");
                                        root.professions_popup.open = false;
                                        root.prof_reset_armed = None;
                                    } else if root.admin_popup.open {
                                        tracing::debug!("закрытие админ-панели: скрим");
                                        root.close_admin();
                                    } else if root.store.npc_dialog.is_some() {
                                        tracing::debug!("закрытие диалога NPC: скрим");
                                        root.store.npc_dialog = None;
                                        root.npc_popup.open = false;
                                    } else if root.map_popup.open {
                                        tracing::debug!("закрытие окна карты: скрим");
                                        root.map_popup.open = false;
                                    } else if root.quests_popup.open {
                                        root.quests_popup.open = false;
                                    } else if root.backpack_popup.open {
                                        root.backpack_popup.open = false;
                                    } else if root.char_popup.open {
                                        root.char_popup.open = false;
                                    }
                                    cx.notify();
                                }
                            }),
                        ),
                )
                .into_any_element(),
        )
    }

    /// M11.2: диспетчер всплывающих диалогов (Ely `Dialog` поверх контента,
    /// ADR-004): экран → приоритеты game > social > sheet.
    pub(super) fn dialog_layer(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        if self.settings_open || self.exit_timer.is_some() {
            return None;
        }
        // M13.3: диалог регистрации живёт на экране входа (D35) — вне
        // гейта active_dialog (тот смотрит только на экраны мира/персонажей).
        if self.register_open {
            return self.register_dialog(cx, pal);
        }
        // Гейт экрана и приоритет — ядро (E2E-тест M11.6); внутри домена
        // порядок полей зеркалит AppStore::active_dialog.
        let kind = self
            .store
            .active_dialog(self.create_open, self.delete_target.is_some())?;
        match kind {
            ld2_core::store::Dialog::Death
            | ld2_core::store::Dialog::Examine
            | ld2_core::store::Dialog::Loot => self.game_dialog(cx, pal),
            ld2_core::store::Dialog::PartyInvite
            | ld2_core::store::Dialog::GuildInvite
            | ld2_core::store::Dialog::Trade => self.social_dialog(cx, pal),
            ld2_core::store::Dialog::Inscribe => self.sheet_dialog(cx, pal),
            ld2_core::store::Dialog::Create | ld2_core::store::Dialog::Delete => {
                self.chars_dialog(cx, pal)
            }
        }
    }

    /// M16/F29h: системные уведомления — стеком вверху по центру.
    /// Тайминг и жизнь тостов — у Toaster (ely-стек показывает их
    /// внизу справа и не позиционируется — здесь только отрисовка).
    pub(super) fn toast_stack(
        &self,
        window: &Window,
        cx: &Context<'_, Self>,
        pal: &Palette,
    ) -> Div {
        let toasts = self.toaster.read(cx).shown();
        if toasts.is_empty() {
            return div();
        }
        let width = 420.0f32;
        let left = ((f32::from(window.viewport_size().width) - width) / 2.0).max(8.0);
        div()
            .absolute()
            .left(px(left))
            .top(px(16.0))
            .w(px(width))
            // Клики по уведомлению не проходят сквозь него (occlude).
            .occlude()
            .flex()
            .flex_col()
            .gap_2()
            .children(toasts.into_iter().map(|(title, _)| {
                div()
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(pal.panel))
                    .border_1()
                    .border_color(rgb(pal.accent))
                    .text_size(px(13.0))
                    .text_center()
                    .child(title.to_string())
            }))
    }

    /// M14/F16: диалог выхода с 20-секундным анимированным таймером
    /// (полоса обновляется тиком 50 мс из tick). В охраняемой зоне
    /// (нет PvP-флага) — кнопка «Выйти сейчас» с мгновенным выходом.
    pub(super) fn exit_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let (kind, deadline) = self.exit_timer?;
        let now = self.now_ms();
        let left_ms = (deadline - now).max(0);
        let frac = (left_ms as f32 / EXIT_DELAY_MS as f32).clamp(0.0, 1.0);
        let secs = (left_ms + 999) / 1000;
        let guarded = self
            .store
            .snapshot
            .as_ref()
            .map(|s| guarded_zone(&s.location.flags))
            .unwrap_or(true);
        let lang = self.lang;
        let title = match kind {
            ExitKind::SwitchChar => t(lang, Key::BtnSwitchChar),
            ExitKind::Logout => t(lang, Key::BtnLogout),
        }
        .to_string();
        let close = self.dismiss(cx, |root| root.exit_timer = None);
        let cancel = act_btn(
            cx,
            t(lang, Key::CancelBtn),
            pal.panel,
            pal.text,
            |this, _cx| {
                this.exit_timer = None;
            },
        );
        let now_btn = guarded.then(|| {
            act_btn(
                cx,
                t(lang, Key::ExitNowBtn),
                pal.danger,
                0xffffff,
                |this, _cx| {
                    if let Some((kind, _)) = this.exit_timer.take() {
                        this.perform_exit(kind);
                    }
                },
            )
        });
        // Полоса: success → warn → danger по мере убывания.
        let bar_color = if frac > 0.5 {
            pal.success
        } else if frac > 0.25 {
            pal.warn
        } else {
            pal.danger
        };
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().child(tf(lang, Key::ExitCountdown, &[("{s}", &secs.to_string())])))
            .child(
                div()
                    .w_full()
                    .h(px(8.0))
                    .rounded_md()
                    .bg(rgb(pal.border))
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative(frac))
                            .rounded_md()
                            .bg(rgb(bar_color)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .justify_end()
                    .child(cancel)
                    .children(now_btn),
            );
        Some(
            Dialog::new("ld2-exit", title, close)
                .child(body)
                .into_any_element(),
        )
    }

    /// R-dnd: слот dragged-предмета (для подсветки зон).
    /// R-dnd: окно подтверждения выброса (Dialog, как выход).
    pub(super) fn drop_confirm_dialog(
        &mut self,
        cx: &mut Context<'_, Self>,
        pal: &Palette,
    ) -> Option<AnyElement> {
        let pending = self.drop_confirm.clone()?;
        let lang = self.lang;
        let title = t(lang, Key::DropConfirmTitle).to_string();
        let close = self.dismiss(cx, |root| root.drop_confirm = None);
        let drop_btn = act_btn(
            cx,
            t(lang, Key::MenuDrop),
            pal.danger,
            0xffffff,
            |this, _cx| {
                if let Some(p) = this.drop_confirm.take() {
                    let cmd = ld2_protocol::cmd::ItemDrop {
                        item_uid: p.uid,
                        qty: None,
                    };
                    if this.send_or_notice(&cmd, SendSlot::Notice) {
                        this.store.on_sheet_sent(Act::Drop)
                    }
                }
            },
        );
        let cancel = act_btn(
            cx,
            t(lang, Key::CancelBtn),
            pal.panel,
            pal.text,
            |this, _cx| {
                this.drop_confirm = None;
            },
        );
        // Стак >1: ввод количества (цифры), ползунок, «Все»,
        // подтверждение шлёт ItemDrop с выбранным qty (заказчик).
        let mut body = div().flex().flex_col().gap_3();
        body = body.child(div().child(tf(
            lang,
            Key::DropConfirmHint,
            &[("{name}", pending.name.as_str())],
        )));
        if pending.max > 1 {
            let max = pending.max as f64;
            let qty = pending.qty.clamp(1, pending.max) as f64;
            let this = cx.entity();
            body = body.child(
                div().w(px(220.0)).child(
                    NumberInput::new("ld2-drop-qty", qty)
                        .precision(0)
                        .step(1.0)
                        .range(1.0, max)
                        .on_change(move |v, _window, acx| {
                            this.update(acx, |root, cx| {
                                if let Some(p) = root.drop_confirm.as_mut() {
                                    p.qty = (v.round() as i64).clamp(1, p.max);
                                }
                                cx.notify();
                            });
                        }),
                ),
            );
            let this = cx.entity();
            body = body.child(
                Slider::new("ld2-drop-qty-slider", qty)
                    .range(1.0, max)
                    .step(1.0)
                    .on_change(move |v, _window, acx| {
                        this.update(acx, |root, cx| {
                            if let Some(p) = root.drop_confirm.as_mut() {
                                p.qty = (v.round() as i64).clamp(1, p.max);
                            }
                            cx.notify();
                        });
                    }),
            );
            // «Все» — мгновенно максимум (заказчик).
            body = body.child(div().flex().flex_row().gap_2().child(act_btn(
                cx,
                t(lang, Key::QtyAll),
                pal.panel,
                pal.text,
                move |this, cx| {
                    // this — уже Root в обновлении; реентерантный
                    // entity.update себя же = краш.
                    if let Some(p) = this.drop_confirm.as_mut() {
                        p.qty = p.max;
                    }
                    cx.notify();
                },
            )));
        }
        body = body.child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap_2()
                .child(cancel)
                .child(drop_btn),
        );
        Some(
            Dialog::new("ld2-drop-confirm", title, close)
                .child(body)
                .into_any_element(),
        )
    }

    /// M16/F29d: запрос контекстного меню экипировки (левый/правый
    /// клик по предмету) — номер растёт, чтобы Ely переоткрыл меню.
    pub(super) fn bump_equip_menu(
        &mut self,
        slot: String,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<'_, Self>,
    ) {
        let n = self.equip_menu.as_ref().map(|(_, n, _)| *n).unwrap_or(0) + 1;
        self.equip_menu = Some((slot, n, at));
        cx.notify();
    }

    /// M16/F29g: запрос контекстного меню предмета рюкзака (левый и
    /// правый клик) — номер растёт, чтобы Ely переоткрыл меню.
    pub(super) fn bump_bp_menu(
        &mut self,
        uid: String,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<'_, Self>,
    ) {
        let n = self.bp_menu.as_ref().map(|(_, n, _)| *n).unwrap_or(0) + 1;
        self.bp_menu = Some((uid, n, at));
        cx.notify();
    }

    /// M14/F15: закрыть настройки (общий путь для Esc/скрима/✕ Ely).
    pub(super) fn close_settings(&mut self) {
        self.settings_open = false;
        self.color_picker_open = None;
    }

    /// M14/F16: открыть диалог выхода с таймером (20 с). Настройки
    /// закрываются — диалог выхода всегда верхний.
    pub(super) fn open_exit(&mut self, kind: ExitKind, cx: &mut Context<'_, Self>) {
        self.settings_open = false;
        self.color_picker_open = None;
        self.exit_timer = Some((kind, self.now_ms() + EXIT_DELAY_MS));
        cx.notify();
    }
}

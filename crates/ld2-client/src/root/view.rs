//! Корневой рендер: шапка, экраны и слои. Вынесено из `root.rs` (RF3j).

use super::*;

impl Render for Root {
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl gpui::IntoElement {
        let pal = palette(&self.config.theme);
        // M16/F29f: вьюпорт кэшируется — базовый размер окна
        // клампится под экран при открытии (в обработчиках нет Window).
        let vp = window.viewport_size();
        self.viewport = (f32::from(vp.width), f32::from(vp.height));
        // Автофокус поля логина на первом кадре (экран входа).
        if self.first_frame {
            self.first_frame = false;
            self.init_inputs(window, cx);
            if self.store.screen == Screen::Login {
                let handle = self.ti(&self.login_input).read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(pal.bg))
            .text_color(rgb(pal.text))
            // M18/F41: клик мимо поля ввода снимает с него фокус
            // (capture-фаза корня — до bubble-фокусировки самого поля:
            // клик по полю вернёт фокус ему, клик мимо оставит снятым).
            // Без этого фокус «залипал» в чате и гейт typing_focused
            // глушил все хоткеи окон после набора текста.
            .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                if this.typing_focused(window, cx) {
                    tracing::debug!("клик мимо поля → снятие фокуса");
                    window.blur(cx);
                    cx.notify();
                }
            }))
            .child(self.titlebar(cx, &pal))
            .child(self.header(cx, &pal))
            .child(
                // Экран — на остаток высоты между шапкой и статус-баром:
                // size_full внутри даёт 100 % этого контейнера, а не окна
                // (иначе низ уходил за границу, баг M11).
                div()
                    .flex()
                    .flex_col()
                    .flex_grow_1()
                    .min_h_0()
                    .child(match self.store.screen {
                        Screen::Login => self.login_screen(window, cx, &pal),
                        Screen::Characters => self.characters_screen(cx, &pal),
                        Screen::Game => self.game_screen(cx, &pal),
                    })
                    // Статус-бар логина/выбора: автор+версия в центре,
                    // статус сервера справа (у игры — свой бар с чатом).
                    .when(self.store.screen != Screen::Game, |d| {
                        d.child(self.meta_status_bar(cx, &pal))
                    }),
            )
            // M14/F1: статус сервера живёт в шапке (status_badge).
            // M11.2: всплывающий диалог поверх контента (скрим + панель).
            // M14/F17: карточка навыка — под игровыми диалогами,
            // но над контентом (настройки/выход рисуются позже).
            .when_some(self.skill_info_dialog(cx, &pal), |d, dialog| {
                d.child(dialog)
            })
            // M16/F29b: общий scrim всплывающих окон — под карточками
            // (персонаж и рюкзак могут быть открыты одновременно).
            .when_some(self.popup_scrim(cx), |d, layer| d.child(layer))
            // M15/F25b: окно персонажа — всплывающий диалог;
            // игровые диалоги/настройки/выход рисуются позже — выше.
            .when_some(self.character_dialog(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M16/F29b: окно рюкзака (открывается вместе с персонажем).
            .when_some(self.backpack_dialog(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M18/F36: окно квестов — поверх рюкзака (порядок слоёв =
            // порядок закрытия по Esc/scrim).
            .when_some(self.quests_dialog(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M26: окно карты мира — поверх квестов (заказчик: кнопка
            // в шапке + клавиша «m»).
            .when_some(self.map_dialog(window, cx, &pal), |d, layer| d.child(layer))
            // M18/F38: окно диалога NPC — верхний слой группы попапов
            // (модалки смерти/осмотра/луута рисуются позже — выше).
            .when_some(self.npc_window(window, cx, &pal), |d, layer| d.child(layer))
            // M18/F53: описание предмета и окно заточки — попапы
            // (карточка с шапкой и крестиком, без затемнения).
            // Порядок слоёв = порядок Esc/scrim: заточка — верхняя.
            .when_some(self.item_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            .when_some(self.upgrade_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M18: окно сокетов (инкрустация через меню оружия) —
            // popup-диалог, как другие окна (карточка с крестиком).
            .when_some(self.socket_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M20: окно выбора количества — поверх окна сокетов.
            .when_some(self.quantity_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M24: админ-панель — под окном диалога (execute-диалог
            // количества должен открываться поверх).
            .when_some(self.admin_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M20: окно игрового диалога (заготовка) — поверх окна
            // количества.
            .when_some(self.dialog_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M21: окно профессий — поверх окна диалога.
            .when_some(self.professions_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            // M23: окно подятия предмета — верхний слой группы попапов.
            .when_some(self.pickup_window(window, cx, &pal), |d, layer| {
                d.child(layer)
            })
            .when_some(self.dialog_layer(cx, &pal), |d, dialog| d.child(dialog))
            // M14/F14: настройки — диалог поверх экрана (скрим + Esc).
            .when_some(self.settings_dialog(cx, &pal), |d, layer| d.child(layer))
            .when_some(self.exit_dialog(cx, &pal), |d, dialog| d.child(dialog))
            // R-dnd: подтверждение выброса (поверх всего).
            .when_some(self.drop_confirm_dialog(cx, &pal), |d, dialog| {
                d.child(dialog)
            })
            // M35: карточка осмотра чужого персонажа — поверх игры,
            // под тостами (клик мимо/Esc закрывают).
            .when_some(self.inspect_popup(cx, &pal), |d, layer| d.child(layer))
            // M16/F29h: системные уведомления — вверху по центру
            // (ely ToastViewport показывает внизу справа и не
            // позиционируется — заменён; тайминг у Toaster).
            .child(self.toast_stack(window, cx, &pal))
    }
}

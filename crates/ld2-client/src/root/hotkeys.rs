//! Горячие клавиши: гейт фокуса набора, ремаппинг действий и разбор
//! спецификаций `alt+up`/`1`…/`c`,`i`,`j`,`p`,`m`. Вынесено из `root.rs` (RF3d).

use super::*;

impl Root {
    /// Есть ли фокус в текстовом поле **текущего экрана** (навигация
    /// не должна срабатывать при наборе текста).
    pub(crate) fn typing_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        TYPING_FIELDS
            .iter()
            .filter(|f| f.screen == self.store.screen)
            .filter_map(|f| (f.get)(self).as_ref())
            .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
            // M24: поля execute-диалога (ввод количества) живут в
            // `dialog_inputs`, а не в TYPING_FIELDS — цифры не должны
            // стрелять хоткеями hotbar.
            || self.dialog_inputs.as_ref().is_some_and(|(_, fields)| {
                fields.iter().any(|(_, input)| {
                    input.read(cx).focus_handle(cx).is_focused(window)
                })
            })
    }

    /// Обработка глобального нажатия: навигация/цикл целей по хоткеям
    /// из config (ФР-19); не срабатывает вне игры и при наборе текста.
    pub(crate) fn handle_keystroke(
        &mut self,
        ks: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // M18/F41: Esc в поле ввода = выход из чата (снять фокус),
        // иначе гейт ниже глушит хоткеи до следующего клика мышью.
        if ks.key == "escape" && self.typing_focused(window, cx) {
            tracing::debug!("Esc в поле ввода → снятие фокуса");
            window.blur(cx);
            cx.notify();
            return;
        }
        // Заказчик 2026-11-20: ↑/↓ в поле чата — листать историю ввода
        // (до гейта typing: фокус именно в поле чата).
        if ks.key == "up" || ks.key == "down" {
            let chat_focus = self
                .chat_input
                .as_ref()
                .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window));
            if chat_focus {
                self.chat_history_nav(ks.key == "up", cx);
                cx.notify();
                return;
            }
        }
        let typing = self.typing_focused(window, cx);
        if self.store.screen != Screen::Game || typing {
            // Диагностика жалоб «хоткеи не работают»: ранний выход из-за
            // экрана или фокуса в поле ввода.
            tracing::debug!(
                "keystroke {:?} пропущен гейтом: screen={:?}, typing_focused={}",
                ks,
                self.store.screen,
                typing
            );
            return;
        }
        for (spec, dir) in [
            (self.config.hotkeys.nav_up.clone(), "north"),
            (self.config.hotkeys.nav_down.clone(), "south"),
            (self.config.hotkeys.nav_left.clone(), "west"),
            (self.config.hotkeys.nav_right.clone(), "east"),
        ] {
            if keystroke_matches(&spec, ks) {
                self.nav_direction(dir);
                cx.notify();
                return;
            }
        }
        if keystroke_matches(&self.config.hotkeys.cycle_target, ks) {
            self.cycle_target();
            cx.notify();
            return;
        }
        // M15/F25/M16: Esc закрывает верхнее из окон — но не поверх
        // настроек/диалога выхода (они ловят Esc сами).
        if self.esc_closes_top(ks, cx) {
            return;
        }
        // M15/F25: хоткей окна персонажа (кнопка в шапке дублирует);
        // M16/F29b: окна открываются одновременно (общий scrim).
        if self.popup_hotkeys(ks, cx) {
            return;
        }
        // M14/F4: боевые действия панели — те же действия, что кнопки
        // (цифры 1–6; Esc занят диалогами → «Побег» = 4).
        if self.action_hotkey(ks, cx) {
            cx.notify();
        }
    }

    /// Esc: закрыть верхнее окно из стека попапов (M15/F25, M18/F53/54,
    /// M20, M21, M23, M24, M26). `true` — нажатие обработано.
    /// Вынесено из `handle_keystroke` (RF3u), тело перенесено как есть.
    fn esc_closes_top(&mut self, ks: &gpui::Keystroke, cx: &mut Context<'_, Self>) -> bool {
        if self.settings_open || self.exit_timer.is_some() || ks.key != "escape" {
            return false;
        }
        // M23: окно подятия — верхний слой группы попапов (рисуется
        // последним поверх всех попапов): Esc закрывает его первым.
        if self.pickup_popup.open {
            tracing::debug!("закрытие окна подятия: Esc");
            self.close_pickup();
            cx.notify();
            return true;
        }
        // M18/F54: окно заточки (список предметов или сама заточка) —
        // верхний слой группы окон; Esc закрывает оба экрана разом.
        if self.store.sharpen_stone.is_some() || self.store.upgrade.is_some() {
            tracing::debug!("закрытие окна заточки: Esc");
            self.store.close_upgrade();
            self.store.sharpen_stone = None;
            self.store.sharpen_list = false;
            self.upgrade_progress = None;
            cx.notify();
            return true;
        }
        // M20: окно количества — верхний слой.
        if self.quantity_pick.is_some() {
            self.quantity_pick = None;
            self.quantity_popup.open = false;
            cx.notify();
            return true;
        }
        // M18: окно сокетов — верхний слой группы попапов; Esc
        // сначала возвращает со второй страницы к списку слотов.
        if self.store.socketing.is_some() {
            if self.socket_pick.is_some() {
                self.socket_pick = None;
                cx.notify();
                return true;
            }
            tracing::info!("закрытие окна сокетов: Esc");
            self.store.close_socketing();
            self.socket_popup.open = false;
            cx.notify();
            return true;
        }
        // M18/F53: описание предмета — поверх окна заточки.
        if self.store.item_detail.is_some() {
            tracing::debug!("закрытие описания предмета: Esc");
            self.store.item_detail = None;
            cx.notify();
            return true;
        }
        // M20: игровой диалог — верхний слой (заготовка).
        if self.store.dialog.is_some() {
            tracing::debug!("закрытие игрового диалога: Esc");
            self.close_game_dialog();
            cx.notify();
            return true;
        }
        // M21: окно профессий (заготовка сброса гаснет вместе).
        if self.professions_popup.open {
            tracing::debug!("закрытие окна профессий: Esc");
            self.professions_popup.open = false;
            self.prof_reset_armed = None;
            cx.notify();
            return true;
        }
        // M24: админ-панель — ниже диалога/профессий в стеке слоёв.
        if self.admin_popup.open {
            tracing::debug!("закрытие админ-панели: Esc");
            self.close_admin();
            cx.notify();
            return true;
        }
        // M18/F38: диалог NPC — верхний слой группы окон.
        if self.store.npc_dialog.is_some() {
            tracing::debug!("закрытие диалога NPC: Esc");
            self.store.npc_dialog = None;
            self.npc_popup.open = false;
            cx.notify();
            return true;
        }
        // M18/F36: квесты рисуются поверх рюкзака — Esc закрывает
        // верхнее (после рассуждений порядок: диалог → квесты →
        // рюкзак → персонаж).
        if self.quests_popup.open {
            self.quests_popup.open = false;
            cx.notify();
            return true;
        }
        if self.backpack_popup.open {
            self.backpack_popup.open = false;
            cx.notify();
            return true;
        }
        if self.char_popup.open {
            self.char_popup.open = false;
            cx.notify();
            return true;
        }
        false
    }

    /// Хоткеи окон персонажа/рюкзака/квестов/профессий/карты (M15/F25,
    /// M16/F29b, M18/F36, M21, M26). `true` — нажатие обработано.
    /// Вынесено из `handle_keystroke` (RF3u), тело перенесено как есть.
    fn popup_hotkeys(&mut self, ks: &gpui::Keystroke, cx: &mut Context<'_, Self>) -> bool {
        // M15/F25: хоткей окна персонажа (кнопка в шапке дублирует);
        // M16/F29b: окна открываются одновременно (общий scrim).
        if keystroke_matches(&self.config.hotkeys.open_character, ks) {
            tracing::debug!("hotkey окна персонажа → open={}", !self.char_popup.open);
            if self.char_popup.open {
                self.char_popup.open = false;
            } else {
                // M16/F29d: базовый размер + первая вкладка.
                self.open_popup(PopupKind::Character);
            }
            cx.notify();
            return true;
        }
        // M16/F29b: хоткей рюкзака (кнопка в шапке дублирует).
        if keystroke_matches(&self.config.hotkeys.open_backpack, ks) {
            if self.backpack_popup.open {
                self.backpack_popup.open = false;
            } else {
                self.open_popup(PopupKind::Backpack);
            }
            cx.notify();
            return true;
        }
        // M18/F36: хоткей окна квестов (кнопка в шапке дублирует).
        if keystroke_matches(&self.config.hotkeys.open_quests, ks) {
            if self.quests_popup.open {
                self.quests_popup.open = false;
            } else {
                self.open_popup(PopupKind::Quests);
            }
            cx.notify();
            return true;
        }
        // M21: хоткей окна профессий («p» — кнопка в шапке дублирует).
        if keystroke_matches(&self.config.hotkeys.open_professions, ks) {
            if self.professions_popup.open {
                self.professions_popup.open = false;
            } else {
                self.open_popup(PopupKind::Professions);
            }
            self.prof_reset_armed = None;
            cx.notify();
            return true;
        }
        // M26: хоткей окна карты («m» — кнопка в шапке дублирует).
        if keystroke_matches(&self.config.hotkeys.open_map, ks) {
            if self.map_popup.open {
                self.map_popup.open = false;
            } else {
                self.open_popup(PopupKind::Map);
            }
            cx.notify();
            return true;
        }
        false
    }

    /// M14/F4: боевой хоткей → то же действие, что кнопка панели.
    /// Во время кулдауна действие не отправляется (молча — как
    /// disabled-кнопка с тултипом). Возвращает true, если спецификация
    /// совпала с одним из боевых действий.
    fn action_hotkey(&mut self, ks: &gpui::Keystroke, cx: &mut Context<'_, Self>) -> bool {
        #[derive(Clone, Copy)]
        enum Action {
            Attack,
            PowerStrike,
            Cleave,
            Flee,
            Rune,
            Consumable,
        }
        let hk = self.config.hotkeys.clone();
        let action = if keystroke_matches(&hk.action_attack, ks) {
            Action::Attack
        } else if keystroke_matches(&hk.action_power_strike, ks) {
            Action::PowerStrike
        } else if keystroke_matches(&hk.action_cleave, ks) {
            Action::Cleave
        } else if keystroke_matches(&hk.action_flee, ks) {
            Action::Flee
        } else if keystroke_matches(&hk.action_rune, ks) {
            Action::Rune
        } else if keystroke_matches(&hk.action_consumable, ks) {
            Action::Consumable
        } else {
            return false;
        };

        // Глобальный кулдаун (1 с) блокирует любое боевое действие.
        let now = self.now_ms();
        if self.store.global_cooldown_left(now) > 0 {
            return true;
        }

        match action {
            Action::Attack => {
                if let Some(id) = self.store.target.clone() {
                    self.on_click(Click::Attack(id), cx);
                }
            }
            Action::PowerStrike => {
                // Собственный кулдаун активки + гейт навыка (§4).
                let own = self.own_cd_left_ms("power_strike", now);
                let open = self
                    .store
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| game::power_strike_open(&s.char.skills));
                if AppStore::action_ready(own, 0).0 && open {
                    self.on_click(Click::PowerStrike, cx);
                }
            }
            // Cleave не реализован: кнопка скрыта, хоткей молчит.
            Action::Cleave => {}
            Action::Flee => self.on_click(Click::Flee, cx),
            Action::Rune => {
                if let Some(click) = self.first_rune_click() {
                    self.on_click(click, cx);
                }
            }
            Action::Consumable => {
                if let Some(uid) = self.first_consumable_uid() {
                    self.on_click(Click::UseItem(uid), cx);
                }
            }
        }
        true
    }
}

/// Совпадает ли нажатие со спецификацией хоткея (`alt+up`, `tab`, …).
/// Спецификации из config.toml парсятся gpui-синтаксисом (`alt-up`).
pub(super) fn keystroke_matches(spec: &str, ks: &gpui::Keystroke) -> bool {
    let normalized = spec.replace('+', "-");
    // gpui-pre 0.3.10: parse требует &'static str — динамические строки
    // из config.toml парсятся через parse_with_os (документированный путь).
    match gpui::Keystroke::parse_with_os(&normalized, host_os()) {
        Ok(want) => {
            want.modifiers.alt == ks.modifiers.alt
                && want.modifiers.control == ks.modifiers.control
                && want.modifiers.shift == ks.modifiers.shift
                && want.modifiers.platform == ks.modifiers.platform
                && want.modifiers.function == ks.modifiers.function
                // M15: одна и та же физическая клавиша на латинице и
                // ЙЦУКЕН («c» на русской раскладке приходит как «с» —
                // charactersIgnoringModifiers отдаёт символ раскладки).
                && (want.key.eq_ignore_ascii_case(&ks.key)
                    || keys_physically_equal(&want.key, &ks.key))
        }
        Err(_) => false,
    }
}

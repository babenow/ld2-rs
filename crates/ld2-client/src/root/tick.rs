//! Тик клиента: опрос ingress-канала, применение эффектов и догрузка
//! запросов (карта, лут, админ-окно). Вынесено из `root.rs` (RF3c).

use super::*;

/// Флаги активности таймеров клиента (RF3c).
#[derive(Default)]
struct TickActive {
    exit: bool,
    inscribe: bool,
    upgrade: bool,
    harvest: bool,
    rune_bar: bool,
    rune_buffs: bool,
    attack_bar: bool,
    stun: bool,
    log: bool,
}

impl Root {
    /// Один шаг фонового цикла: дренаж ingress (≤64), исполнение эффектов,
    /// Enter в полях входа, бейдж обновлений. Возвращает задержку до
    /// следующего шага (НФР §8: события → 50 мс, idle → 200 мс ≤ 5 fps).
    pub(crate) fn tick(&mut self, cx: &mut Context<'_, Self>) -> Duration {
        let mut had_events = false;
        for _ in 0..64 {
            match self.ingress.try_recv() {
                Ok(ev) => {
                    had_events = true;
                    let effects = self.store.apply(ev);
                    self.exec_effects(effects);
                }
                Err(_) => break,
            }
        }
        self.tick_drag(cx);
        self.tick_pumps(cx);
        // Результат фоновой проверки обновлений (§12).
        while let Ok(version) = self.update_rx.try_recv() {
            if ld2_core::update_check::is_newer(&version, env!("CARGO_PKG_VERSION")) {
                tracing::info!("доступна версия v{version}");
                self.update_available = Some(version);
            }
        }

        let t = self.tick_timers(cx);
        // M11.4: новый notice → всплывающий тост Ely (текст в экране
        // остаётся до замены — на время жизни тоста возможен дубль).
        if let Some(notice) = self.store.notice.clone() {
            if self.last_notice.as_deref() != Some(notice.as_str()) {
                self.last_notice = Some(notice.clone());
                let toast = Toast::new(notice);
                self.toaster.update(cx, |t, cx| {
                    t.push(toast, cx);
                });
            }
        } else {
            self.last_notice = None;
        }

        // M14/F4: пока есть активный кулдаун (глобальный или любой
        // собственный) — тик 50 мс, чтобы отсчёт на кнопках шёл плавно.
        let now_ms = self.now_ms();
        let cd_active = self.store.global_cooldown_left(now_ms) > 0
            || self
                .store
                .combat_cooldowns
                .values()
                .any(|until_s| until_s.saturating_mul(1000) > now_ms)
            // M17/F30g: анимация прогресс-баров предметных кнопок.
            || self
                .store
                .item_cooldowns
                .values()
                .any(|(until_ms, _)| *until_ms > now_ms)
            // M18/F42: живой отсчёт таймеров реплик на кулдауне.
            || self.store.npc_dialog.as_ref().is_some_and(|dialog| {
                dialog.node.as_ref().is_some_and(|node| {
                    node.replies.iter().any(|reply| {
                        reply
                            .wait_until_ms
                            .is_some_and(|until| until > now_ms)
                    })
                })
            })
            // M18/F53: прогресс улучшения в диалоге.
            || self.upgrade_progress.is_some()
            // M36: обратный отсчёт AFK-предупреждения.
            || self.store.afk_until_ms.is_some();

        if had_events
            || cd_active
            || t.exit
            || t.inscribe
            || t.upgrade
            || t.harvest
            || t.rune_bar
            || t.rune_buffs
            || t.attack_bar
            || t.stun
            || t.log
        {
            // gpui перерисовывается только по notify (аналог request_repaint).
            cx.notify();
        }
        Duration::from_millis(
            if had_events
                || cd_active
                || t.exit
                || t.inscribe
                || t.upgrade
                || t.harvest
                || t.rune_bar
                || t.rune_buffs
                || t.attack_bar
            {
                50
            } else {
                200
            },
        )
    }

    /// R-dnd: завершение drag вне зон → подтверждение выброса (RF3c).
    fn tick_drag(&mut self, cx: &mut Context<'_, Self>) {
        // R-dnd: drag закончился и НИ ОДНА зона не поглотила его своим
        // on_drop (hover hit-test — работает для drag из любого места,
        // в отличие от on_drag_move, который срабатывает только у
        // элемента, где drag начался) — отпускание вне зон:
        // подтверждение выброса (заказчик).
        // R-dnd: drag закончился и НИ ОДНА зона не поглотила его своим
        // on_drop (hover hit-test — работает для drag из любого места,
        // в отличие от on_drag_move, который срабатывает только у
        // элемента, где drag начался) — отпускание вне зон:
        // подтверждение выброса (заказчик).
        if let Some(drag) = self.drag_item.take() {
            if !cx.has_active_drag() {
                tracing::info!("dnd: конец drag вне зон → подтверждение выброса");
                // Стак: max — сколько в стаке (1 — одиночный).
                let found = self
                    .store
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.char.inventory.iter().find(|i| i.uid == drag.uid))
                    .map(|i| (i.name.clone(), i.qty.unwrap_or(1).max(1)));
                let (name, max) = found.unwrap_or_else(|| (drag.uid.clone(), 1));
                self.drop_confirm = Some(DropPending {
                    uid: drag.uid.clone(),
                    name,
                    from: drag.from,
                    max,
                    qty: max,
                });
                self.bp_hold = None;
                cx.notify();
            } else {
                self.drag_item = Some(drag);
            }
        }
    }

    /// Догрузки при свободном ack-слоте: лут, админ-каталог, карта
    /// мира, перекалибровка часов (RF3c).
    fn tick_pumps(&mut self, cx: &mut Context<'_, Self>) {
        // R16 (§1.1): перекалибровка часов каждые 5 мин — сервер
        // отвечает evt/time.sync; медиану держит стор.
        let recalibrate = self
            .last_time_sync
            .map(|t| t.elapsed() >= std::time::Duration::from_secs(300))
            .unwrap_or(true);
        if recalibrate {
            self.last_time_sync = Some(std::time::Instant::now());
            let _ = self.try_send(&TimeSync {});
        }
        // M23: лут трупа обновился (разделка/создание трупа) —
        // перечитываем, пока окно подятия открыто.
        if self.store.pickup_dirty {
            self.store.pickup_dirty = false;
            if self.pickup_popup.open {
                self.pickup_request = true;
            }
        }
        // M23: очередь «Взять всё» — по одному взятию за свободный
        // ack-слот; лут окна перечитывается после неё.
        if !self.pickup_queue.is_empty() {
            self.pump_take_queue(cx);
        }
        // M23: окно подятия — догрузка/обновления лута, когда
        // транспорт свободен (см. `pickup_request`).
        if self.pickup_request && self.pickup_queue.is_empty() {
            self.pump_pickup(cx);
        }
        // M24: админ-панель — догрузка каталога и содержимого цели
        // при свободном ack-слоте.
        if self.admin_catalog_request {
            self.pump_admin_catalog(cx);
        }
        // M26: карта мира — одна догрузка чанков на сессию (насос;
        // apply_ack ставит map_request на следующий чанк).
        if !self.map_started && self.store.screen == Screen::Game {
            self.map_started = true;
            self.store.map_request = true;
        }
        if self.store.map_request {
            self.pump_map_world(cx);
        }
        // M26: карта загружена целиком → компасная раскладка (один раз).
        if !self.map_pos_ready
            && self.store.map_total > 0
            && self.store.map_offset >= self.store.map_total
        {
            self.map_pos = crate::map::layout(&self.store.map_locations, &self.store.map_root);
            self.map_pos_ready = true;
            tracing::info!("dnd/карта: раскладка {} узлов", self.map_pos.len());
            cx.notify();
        }
        if self.admin_target_request {
            self.pump_admin_target(cx);
        }
    }

    /// Активные таймеры/прогресс-бары: возвращает, что нужно
    /// перерисовать и держать быстрый тик 50 мс (RF3c).
    fn tick_timers(&mut self, cx: &mut Context<'_, Self>) -> TickActive {
        let mut a = TickActive::default();
        // M11.7: таймер удаления (D20): тикает только на экране персонажей;
        // вход в игру/логаут (смена экрана) → отмена; истёк → char.delete.
        if self.store.pending_delete.is_some() {
            if self.store.screen != Screen::Characters {
                self.store.cancel_delete();
            } else if let Some(char_id) = self.store.tick_delete(local_epoch()) {
                let cmd = CharDelete {
                    char_id: char_id.clone(),
                };
                if self.send_or_notice(&cmd, SendSlot::Notice) {
                    self.store.on_delete_sent(&char_id)
                }
            }
            // Отсчёт на карточке — перерисовка каждый тик.
            cx.notify();
        }

        // M14/F16: таймер выхода — полоса анимируется (тик 50 мс),
        // по истечении 20 с действие выполняется автоматически.
        a.exit = self.exit_timer.is_some();
        if let Some((kind, deadline)) = self.exit_timer {
            if self.now_ms() >= deadline {
                self.perform_exit(kind);
            }
        }

        // M17/F30a: каст-бар инкрустации (3 с, без отмены) — по
        // завершении отправляем socket.insert в первый свободный сокет.
        a.inscribe = self.cast_inscribe.is_some();
        if let Some((rune_uid, target_uid, ends)) = self.cast_inscribe.clone() {
            if self.now_ms() >= ends {
                self.cast_inscribe = None;
                self.finish_inscribe(rune_uid, target_uid);
            }
        }

        // M18/F53: прогресс улучшения в диалоге — по истечении 3 с
        // (из настроек сервера) отправляем cmd/upgrade.start; модалка
        // закроется по evt/upgrade/result.
        a.upgrade = self.upgrade_progress.is_some();
        // M22: прогресс-бар разделки (5 с) должен идти плавно —
        // держим быстрый тик (50 мс) с notify, пока он активен.
        a.harvest = self.store.harvest.is_some();
        // M24: прогрессбары зарядов рун живут по времени — тик,
        // пока хотя бы одна руна в надетом снаряжении не заполнена.
        a.rune_bar = self.rune_bar_active();
        // M24: индикаторы бафов рун — живой отсчёт и моргание.
        a.rune_buffs = self.rune_buffs_active();
        // M25: бары атак монстров — живая заливка до следующей атаки.
        a.attack_bar = self.attack_bars_active();
        // M25: стан истёк — закрываем всплывающее окно.
        a.stun = self
            .store
            .stun
            .as_ref()
            .is_some_and(|s| self.now_ms() >= s.until_ms);
        if a.stun {
            self.store.stun = None;
        }
        // M25: журнал локации исчезающий — строки старше TTL сервера
        // (`journal_ttl_s`, по умолчанию 30 с) удаляются.
        let log_ttl_s = self
            .store
            .session
            .as_ref()
            .map(|s| s.journal_ttl_s)
            .unwrap_or(30);
        a.log = self.store.expire_log(local_epoch_ms(), log_ttl_s);
        // M24 фикс: 4-й элемент — **длительность** (док поля), но
        // сравнивался с epoch-мс как дедлайн → старт уходил мгновенно
        // и прогресс-бар заточки не был виден. Теперь: старт + длительность.
        if let Some((uid, preview_id, started, duration)) = self.upgrade_progress.clone() {
            // M18/F54: камень мог выбыть за 3 с прогресса — старт без
            // него сервер отверг бы; сбрасываем, ничего не отправляя.
            if self.store.sharpen_stone.is_none() {
                tracing::debug!("прогресс заточки сброшен: камень выбыл");
                self.upgrade_progress = None;
            } else if self.now_ms() >= started + duration {
                self.upgrade_progress = None;
                let cmd = ld2_protocol::cmd::UpgradeStart {
                    item_uid: uid,
                    preview_id,
                };
                self.send_cmd(&cmd, Act::Upgrade);
            }
        }
        // Если модалка закрыта (Esc/✕) — сбрасываем прогресс.
        if self.store.upgrade.is_none() && self.store.sharpen_stone.is_none() {
            self.upgrade_progress = None;
        }

        // M24: единый прогресс-бар левой колонки — гаснет по
        // исчезновению своего источника (результат/отказ/закрытие).
        if let Some(p) = self.cast_progress.as_ref() {
            let gone = match p.source {
                ProgressSource::Harvest => self.store.harvest.is_none(),
                ProgressSource::Inscribe => self.cast_inscribe.is_none(),
                ProgressSource::Sharpen => self.upgrade_progress.is_none(),
            };
            if gone {
                self.cast_progress = None;
            }
        }

        // M18/F53: после результата модалка остаётся открытой —
        // автоматически запрашиваем новое превью для следующего улучшения.
        if self.store.sharpen_stone.is_some()
            && self.store.pending != Some(Act::Preview)
            && self
                .store
                .upgrade
                .as_ref()
                .is_some_and(|v| v.result.is_some() && v.preview.is_none())
        {
            let uid = self.store.upgrade.as_ref().unwrap().item_uid.clone();
            let cmd = ld2_protocol::cmd::UpgradePreview { item_uid: uid };
            self.send_cmd(&cmd, Act::Preview);
        }

        a
    }

    pub(super) fn exec_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::PersistSession {
                    login,
                    refresh_token,
                } => {
                    if login.trim().is_empty() {
                        continue;
                    }
                    // R5: запись с таймаутом — диалог macOS не вешает UI.
                    if let Err(e) = ld2_core::secrets::set_token_with_timeout(
                        &self.config.host,
                        &login,
                        &refresh_token,
                        std::time::Duration::from_secs(2),
                    ) {
                        tracing::warn!("keyring: не удалось сохранить токен: {e}");
                    }
                    // M16/F29j: запомненный пароль — только в keyring.
                    // Стэш пуст при resume (вход по токену) — старую
                    // запись не трогаем; выключенная галочка её стирает.
                    let pending = self.pending_password.take();
                    if self.config.remember_password {
                        if let Some(pw) = pending {
                            if let Err(e) = ld2_core::secrets::set_password_with_timeout(
                                &self.config.host,
                                &login,
                                &pw,
                                std::time::Duration::from_secs(2),
                            ) {
                                tracing::warn!("keyring: не удалось сохранить пароль: {e}");
                            }
                        }
                    } else if pending.is_some() {
                        if let Err(e) = ld2_core::secrets::delete_password_with_timeout(
                            &self.config.host,
                            &login,
                            std::time::Duration::from_secs(2),
                        ) {
                            tracing::warn!("keyring: не удалось удалить пароль: {e}");
                        }
                    }
                    self.config.last_login = Some(login);
                    if let Err(e) = self.config_store.save(&self.config) {
                        tracing::warn!("не удалось сохранить конфиг: {e:#}");
                    }
                }
                Effect::ForgetSession { login } => {
                    // R5: удаление с таймаутом — диалог macOS не вешает UI.
                    match ld2_core::secrets::delete_token_with_timeout(
                        &self.config.host,
                        &login,
                        std::time::Duration::from_secs(2),
                    ) {
                        Ok(()) | Err(keyring::Error::NoEntry) => {
                            tracing::info!("keyring: токен удалён ({login})");
                        }
                        Err(e) => tracing::warn!("keyring: не удалось удалить токен: {e}"),
                    }
                }
                Effect::TransportToken(token) => {
                    if let Err(e) = self.transport.set_refresh_token(Some(token)) {
                        tracing::warn!("транспорт недоступен для ротации токена: {e}");
                    }
                }
                Effect::ResyncState => {
                    // R11: дельта неприменима/фатальная ошибка — полный
                    // снапшот (§12.4); 0 → сервер отвечает целиком.
                    let version = self
                        .store
                        .snapshot
                        .as_ref()
                        .map(|s| s.state_version)
                        .unwrap_or(0);
                    let _ = self.try_send(&StateResync {
                        last_state_version: version,
                    });
                }
                Effect::Notify(kind) => {
                    // M18/F55: итог заточки рисуется цветом прямо на
                    // экране заточки — системное уведомление («новое
                    // окно») для него убрано. Остальные события (D7) —
                    // под флагом desktop.notify.
                    if matches!(kind, ld2_core::store::NotifyKind::Upgrade) {
                        continue;
                    }
                    if self.config.notify {
                        ld2_core::notify::show(&l10n::notify_body(self.lang, kind));
                    }
                }
            }
        }
    }

    /// M23: запрос данных окна подятия (`cmd/entity.info` →
    /// `Act::PickupInfo`) — только при свободном `pending` (см.
    /// `pickup_request`); иначе ждёт следующего тика.
    pub(super) fn pump_pickup(&mut self, cx: &mut Context<'_, Self>) {
        if !self.pickup_request || !self.pickup_popup.open {
            return;
        }
        if self.store.pending.is_some() {
            return; // ack-слот занят — ждём
        }
        let Some(id) = self.pickup_id.clone() else {
            self.pickup_request = false;
            return;
        };
        let cmd = ld2_protocol::cmd::EntityInfo { entity_id: id };
        if self.send_or_notice(&cmd, SendSlot::World) {
            self.store.on_world_sent(Act::PickupInfo);
            cx.notify();
        }
        self.pickup_request = false;
    }

    /// M23: очередь «Взять всё» — следующий `item.use`, когда
    /// транспорт свободен (корреляция ack-ов одиночная); после
    /// отправки ставится `pickup_request` — когда очередь опустеет,
    /// лут окна освежится обычным перезапросом.
    pub(super) fn pump_take_queue(&mut self, cx: &mut Context<'_, Self>) {
        if self.pickup_queue.is_empty() || !self.pickup_popup.open {
            return;
        }
        if self.store.pending.is_some() {
            return; // ack-слот занят — ждём
        }
        let uid = self.pickup_queue.remove(0);
        let cmd = ld2_protocol::cmd::ItemUse {
            item_uid: uid,
            target_id: None,
        };
        if self.send_or_notice(&cmd, SendSlot::World) {
            self.store.on_world_sent(Act::Drop);
            self.pickup_request = true;
            cx.notify();
        } else {
            self.pickup_queue.clear();
        }
    }

    /// M26: догрузка карты мира — следующий чанк `cmd/map.world`
    /// при свободном `pending` (корреляция ack-ов стора одиночная);
    /// после ack apply_ack ставит map_request на продолжение.
    pub(super) fn pump_map_world(&mut self, cx: &mut Context<'_, Self>) {
        if !self.store.map_request {
            return;
        }
        if self.store.pending.is_some() {
            return; // ack-слот занят — ждём
        }
        let cmd = ld2_protocol::cmd::MapWorld {
            offset: self.store.map_offset,
        };
        if self.send_or_notice(&cmd, SendSlot::Notice) {
            self.store.on_world_sent(Act::MapWorld);
            cx.notify();
        }
        self.store.map_request = false;
    }

    /// M24: каталог админ-окна — `cmd/admin.items` при свободном
    /// `pending` (корреляция ack-ов стора одиночная), как у окна
    /// подятия; после отправки флаг гаснет.
    pub(super) fn pump_admin_catalog(&mut self, cx: &mut Context<'_, Self>) {
        if !self.admin_catalog_request || !self.admin_popup.open {
            return;
        }
        if self.store.pending.is_some() {
            return; // ack-слот занят — ждём
        }
        let cmd = ld2_protocol::cmd::AdminItems {};
        if self.send_or_notice(&cmd, SendSlot::Notice) {
            self.store.on_world_sent(Act::AdminCatalog);
            cx.notify();
        }
        self.admin_catalog_request = false;
    }

    /// M24: содержимое цели правой колонки — `cmd/admin.target.info`
    /// при свободном `pending` (как каталог); после отправки флаг гаснет.
    pub(super) fn pump_admin_target(&mut self, cx: &mut Context<'_, Self>) {
        if !self.admin_target_request || !self.admin_popup.open {
            return;
        }
        if self.store.pending.is_some() {
            return; // ack-слот занят — ждём
        }
        let cmd = ld2_protocol::cmd::AdminTargetInfo {
            target: self.admin_target.clone(),
        };
        if self.send_or_notice(&cmd, SendSlot::Notice) {
            self.store.on_world_sent(Act::AdminTarget);
            cx.notify();
        }
        self.admin_target_request = false;
    }
}

//! Редьюсеры сессионных и системных событий — вынесено из `apply_event` (RF4j).

use super::super::*;

impl AppStore {
    /// Системные события: `time.sync`, `session.start`, `state/*`, `dialog/open`, `error` (RF4j).
    pub(crate) fn reduce_misc(
        &mut self,
        msg_type: &str,
        payload: serde_json::Value,
    ) -> Vec<Effect> {
        match msg_type {
            "evt/time.sync" => {
                // §1.1: server = local + offset — база всех обратных отсчётов.
                let Ok(sync) = serde_json::from_value::<TimeSyncEvent>(payload) else {
                    return vec![];
                };
                let local = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                // R16 (§1.1 «КРИЧНО»): медиана последних 5 замеров;
                // перекалибровка — периодический cmd/time.sync (root).
                self.time_samples.push_back(sync.server_ts - local);
                if self.time_samples.len() > 5 {
                    self.time_samples.pop_front();
                }
                let mut sorted: Vec<i64> = self.time_samples.iter().copied().collect();
                sorted.sort_unstable();
                self.time_offset = sorted[sorted.len() / 2];
                vec![]
            }
            "evt/session.start" => {
                let Ok(cfg) = serde_json::from_value::<SessionConfig>(payload) else {
                    tracing::warn!("некорректный evt/session.start — игнор");
                    // R21: login-ack уже ok — без notice UI висит молча.
                    self.notice = Some(
                        "⚠ Сервер прислал некорректную конфигурацию сессии — переподключитесь"
                            .into(),
                    );
                    return vec![];
                };
                let refresh = cfg.session.refresh.token.clone();
                // НФР «Обновления»: force-min-version capability сервера (§12).
                let required = cfg.server.min_client.clone();
                let login = self
                    .session_login
                    .clone()
                    .unwrap_or_else(|| self.last_login.clone().unwrap_or_default());
                let was_login_screen = self.screen == Screen::Login;
                self.session = Some(cfg);
                self.login_error = None;
                if was_login_screen {
                    // Новый вход — на выбор персонажей; при реконнекте из
                    // игры экран НЕ меняем (§4.5: session.start повторяется).
                    self.screen = Screen::Characters;
                    self.notice = None;
                }
                // После сброса notice: предупреждение о версии не должно
                // затираться переходом экрана (НФР force-min-version).
                // R3: не только notice — жёсткий флаг блокирует вход в
                // мир (chars.rs не отправляет char.select).
                let forced = required
                    .as_deref()
                    .map(|r| crate::update_check::is_newer(r, env!("CARGO_PKG_VERSION")))
                    .unwrap_or(false);
                self.min_client_required = required.clone();
                self.forced_update = forced;
                if let Some(required) = required {
                    if forced {
                        self.notice = Some(format!(
                            "⚠ Сервер требует клиент v{required} (у вас v{}) — обновитесь",
                            env!("CARGO_PKG_VERSION")
                        ));
                    }
                }
                vec![
                    Effect::PersistSession {
                        login,
                        refresh_token: refresh.clone(),
                    },
                    Effect::TransportToken(refresh),
                ]
            }
            "evt/state/snapshot" => {
                // M24: снапшот — новая фаза такта зарядов (серверную
                // фазу клиент не знает; по патчу тикера самокоррекция).
                self.rune_recharge_phase_ms = Some(local_epoch_ms() + self.time_offset * 1000);
                let Ok(snap) = serde_json::from_value::<StateSnapshot>(payload) else {
                    tracing::warn!("некорректный evt/state/snapshot — игнор");
                    // R21: экран мира без снапшота — молчаливое «пустое
                    // окно»; сообщаем пользователю.
                    self.notice = Some(
                        "⚠ Сервер прислал некорректный снапшот мира — переподключитесь".into(),
                    );
                    return vec![];
                };
                self.push_visited(&snap.location);
                // M14/F18: лог мира персональный — очистка при входе в игру
                // (старые строки сессии не переживают вход; строка «Вход в мир»
                // остаётся первой).
                self.log.clear();
                self.push_log(
                    LogKind::Location,
                    format!("Вход в мир: {}", snap.location.name),
                );
                // §7: снапшот несёт межигровое состояние (пати/гильдия).
                self.party = snap.party.clone();
                self.guild = snap.guild.clone();
                self.snapshot = Some(snap);
                self.screen = Screen::Game;
                self.select_error = None;
                self.world_error = None;
                self.entity_detail = None;
                self.pickup = None;
                self.notice = None;
                vec![]
            }
            "evt/dialog/open" => {
                // M20: игровой диалог (заготовка) — гейт рендера окна
                // (`PopupKind::Dialog`) — наличие состояния, как у NPC.
                let Ok(open) = serde_json::from_value::<ld2_protocol::evt::DialogOpen>(payload)
                else {
                    return vec![];
                };
                self.dialog = Some(open);
                vec![]
            }
            "evt/state/delta" => {
                // R11 (§3/§12.4): после state.resync сервер может ответить
                // дельтой — применяем JSON-patch к снапшоту; база не
                // совпала, неприменимый путь или битый результат →
                // полный resync (иначе клиент навсегда на устаревшем
                // состоянии).
                let Ok(delta) = serde_json::from_value::<StateDelta>(payload) else {
                    tracing::warn!("некорректный evt/state/delta → resync");
                    return self.request_resync();
                };
                let Some(snap) = self.snapshot.as_ref() else {
                    return self.request_resync();
                };
                if snap.state_version != delta.from_version {
                    tracing::warn!(
                        "delta: база {} ≠ снапшот {} → resync",
                        delta.from_version,
                        snap.state_version
                    );
                    return self.request_resync();
                }
                let mut value = match serde_json::to_value(snap) {
                    Ok(value) => value,
                    Err(_) => return self.request_resync(),
                };
                if !apply_json_patch(&mut value, &delta.patch) {
                    tracing::warn!("delta: неприменимый патч → resync");
                    return self.request_resync();
                }
                match serde_json::from_value::<StateSnapshot>(value) {
                    Ok(snap) => {
                        self.snapshot = Some(snap);
                        vec![]
                    }
                    Err(_) => {
                        tracing::warn!("delta: патч дал некорректный снапшот → resync");
                        self.request_resync()
                    }
                }
            }
            "evt/error" => {
                // R11 (§8): не-фатальные ошибки сервера — notice;
                // фатальная — notice + resync (восстановление состояния).
                let Ok(err) = serde_json::from_value::<ErrorNotice>(payload) else {
                    return vec![];
                };
                self.notice = Some(format!("⚠ {}: {}", err.code, err.message));
                if err.fatal == Some(true) {
                    return vec![Effect::ResyncState];
                }
                vec![]
            }
            _ => vec![],
        }
    }
}

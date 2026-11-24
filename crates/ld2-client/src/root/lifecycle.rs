//! Жизненный цикл подключения и сессии `Root`: отправка с уведомлением,
//! вход/выход/переподключение, сохранение конфига и действия выхода.
//! Вынесено из `root.rs` (RF3j).

use super::*;

impl Root {
    /// Отправить команду серверу, вернув ошибку связи текстом.
    pub(crate) fn try_send<C: Command>(&mut self, cmd: &C) -> Result<(), String> {
        self.transport
            .send(cmd)
            .map(|_| ())
            .map_err(|e| format!("нет связи: {e}"))
    }

    /// Отправить команду серверу; при обрыве связи — сообщение в нужный слот.
    /// `true` — команда ушла, можно обновлять локальное состояние.
    pub(crate) fn send_or_notice<C: Command>(&mut self, cmd: &C, slot: SendSlot) -> bool {
        match self.try_send(cmd) {
            Ok(()) => true,
            Err(msg) => {
                match slot {
                    SendSlot::Notice => self.store.notice = Some(msg),
                    SendSlot::World => self.store.world_error = Some(msg),
                    SendSlot::Login => self.store.login_error = Some(msg),
                    SendSlot::Create => self.store.create_error = Some(msg),
                    SendSlot::Select => self.store.select_error = Some(msg),
                }
                false
            }
        }
    }

    pub(crate) fn send_login(&mut self, cx: &Context<'_, Self>) {
        let login = self.ti(&self.login_input).read(cx).text().to_string();
        let password = self.ti(&self.password_input).read(cx).text().to_string();
        if login.trim().is_empty() || password.is_empty() {
            self.store.login_error = Some(t(self.lang, Key::ErrFillFields).to_string());
            return;
        }
        // M16/F29j: пароль до move в cmd — для keyring при успешном
        // входе (PersistSession).
        self.pending_password = Some(password.clone());
        let cmd = AuthLogin {
            login: login.clone(),
            password,
        };
        if self.send_or_notice(&cmd, SendSlot::Login) {
            self.store.on_login_sent(&login)
        }
    }

    pub(crate) fn logout(&mut self) {
        let _ = self.try_send(&ld2_protocol::cmd::AuthLogout {});
        let effects = self.store.logout();
        self.exec_effects(effects);
    }

    /// Смена хоста в настройках (ФР-19): переподключение транспорта.
    /// Токены keyring привязаны к адресу — на новом сервере чистый вход.
    pub(crate) fn reconnect(&mut self) {
        tracing::info!("смена сервера → переподключение: {}", self.config.host);
        self.transport.close();
        let (tx, rx) = mpsc::unbounded_channel();
        self.transport = transport::spawn(
            TransportConfig::new(self.config.host.clone()).with_domain(self.config.domain.clone()),
            tx,
        );
        self.ingress = rx;
        self.store.to_login_screen();
    }

    /// Поле, созданное на первом кадре (паникующий доступ: render всегда
    /// идёт после инициализации).
    pub(crate) fn ti<'s>(&self, slot: &'s Option<Entity<TextInput>>) -> &'s Entity<TextInput> {
        slot.as_ref().expect("поле создано на первом кадре")
    }

    /// M18/F41: снять фокус со всех полей ввода окна (`Window::blur`).
    ///
    /// gpui не blur'ит поле само: после набора текста фокус «залипал»
    /// в чате, и гейт [`Self::typing_focused`] глушил **все** глобальные
    /// хоткеи (C/I/квесты не открывались, в т.ч. параллельно). Прямой
    /// `handle.update` из обработчика ввода недоступен: событие
    /// обрабатывается внутри `update_window`, где окно временно снято
    /// с карты `App` (вернётся «window not found»). Откладываем blur
    /// до конца цикла эффектов — к `flush_effects` окно уже на месте.
    pub(crate) fn blur_typing(cx: &mut Context<'_, Self>) {
        cx.defer(|cx| {
            let Some(handle) = cx.active_window() else {
                return;
            };
            let _ = handle.update(cx, |_, window, cx| window.blur(cx));
        });
    }

    pub(crate) fn save_config(&mut self) {
        if let Err(e) = self.config_store.save(&self.config) {
            tracing::warn!("не удалось сохранить конфиг: {e:#}");
        }
    }

    /// M11.2: on_close диалога — мутация состояния Root и перерисовка
    /// (диалог рендерится из состояния, поэтому «закрыть» = снять флаг).
    pub(crate) fn dismiss(
        &self,
        cx: &mut Context<'_, Self>,
        f: impl Fn(&mut Self) + 'static,
    ) -> impl Fn(&mut Window, &mut App) + 'static {
        let this = cx.entity();
        move |_window, acx| {
            this.update(acx, |root, rcx| {
                f(root);
                rcx.notify();
            });
        }
    }

    /// M14/F16: выполнить выход — по истечении таймера или «Выйти сейчас».
    pub(crate) fn perform_exit(&mut self, kind: ExitKind) {
        self.exit_timer = None;
        match kind {
            ExitKind::SwitchChar => self.store.back_to_characters(),
            ExitKind::Logout => self.logout(),
        }
    }
}

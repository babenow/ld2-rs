//! Экраны M2: вход, выбор персонажей (с модалками создания/удаления),
//! заглушка игрового экрана. egui 0.37: корневой `Ui` приходит в `App::ui`.

use eframe::egui;
use ld2_protocol::cmd::{AuthLogin, AuthLogout, CharCreate, CharDelete, CharSelect};

use crate::l10n::{t, tf, Key, Lang};
use crate::store::Act;
use crate::transport::TransportStatus;

use super::{Profile, ScreenEffects, UiCtx};

const RED: egui::Color32 = egui::Color32::from_rgb(0xE0, 0x6C, 0x75);
const ORANGE: egui::Color32 = egui::Color32::from_rgb(0xD1, 0x9A, 0x66);
const FORM_WIDTH: f32 = 300.0;

pub fn status_text(lang: Lang, status: &TransportStatus) -> String {
    let key = match status {
        TransportStatus::Connecting => Key::StatusConnecting,
        TransportStatus::Online => Key::StatusOnline,
        TransportStatus::Reconnecting { .. } => Key::StatusReconnecting,
        TransportStatus::AuthExpired => Key::StatusAuthExpired,
        TransportStatus::Stopped => Key::StatusStopped,
    };
    let base = t(lang, key);
    match status {
        TransportStatus::Reconnecting { attempt, delay } => {
            format!("{base} (попытка {attempt}, {delay:?})")
        }
        _ => base.to_string(),
    }
}

// ─── Экран входа (ФР-01) ────────────────────────────────────────────────────

pub fn login(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui) -> ScreenEffects {
    let lang = ctx.lang;
    ui.vertical_centered(|ui| {
        ui.add_space(56.0);
        ui.heading(t(lang, Key::LoginTitle));
        ui.add_space(6.0);
        ui.weak(status_text(lang, &ctx.store.transport));
        ui.add_space(16.0);

        let login_resp = ui.add(
            egui::TextEdit::singleline(&mut ctx.forms.login.login)
                .hint_text(t(lang, Key::FieldLogin))
                .desired_width(FORM_WIDTH),
        );
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::singleline(&mut ctx.forms.login.password)
                .password(true)
                .hint_text(t(lang, Key::FieldPassword))
                .desired_width(FORM_WIDTH),
        );
        ui.add_space(8.0);

        if let Some(err) = &ctx.store.login_error {
            ui.colored_label(RED, err);
        }
        if let Some(notice) = &ctx.store.notice {
            ui.weak(notice);
        }

        let pending = ctx.store.pending == Some(Act::Login);
        let filled =
            !ctx.forms.login.login.trim().is_empty() && !ctx.forms.login.password.is_empty();
        if ui
            .add_enabled(
                !pending && filled,
                egui::Button::new(t(lang, Key::BtnLogin)),
            )
            .clicked()
        {
            submit_login(ctx);
        }
        if pending {
            ui.add_space(4.0);
            ui.weak(t(lang, Key::Loading));
        }

        // Информационный подвал M0 (сервер/версия) — сохраняем.
        ui.add_space(24.0);
        ui.weak(format!(
            "{}: {} · {} {}",
            t(lang, Key::Host),
            host_of(ctx),
            t(lang, Key::Version),
            env!("CARGO_PKG_VERSION")
        ));
        let _ = login_resp;
    });
    vec![]
}

fn host_of(ctx: &UiCtx<'_>) -> String {
    ctx.store
        .session
        .as_ref()
        .map(|s| s.server.name.clone())
        .unwrap_or_else(|| "—".into())
}

fn submit_login(ctx: &mut UiCtx<'_>) {
    let login = ctx.forms.login.login.trim().to_string();
    let password = ctx.forms.login.password.clone();
    let cmd = AuthLogin {
        login: login.clone(),
        password,
    };
    match ctx.transport.send(&cmd) {
        Ok(_) => {
            ctx.store.on_login_sent(&login);
            ctx.forms.login.password.clear();
        }
        Err(e) => ctx.store.login_error = Some(format!("нет связи с сервером: {e}")),
    }
}

// ─── Экран выбора персонажей (ФР-02) ────────────────────────────────────────

pub fn characters(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui) -> ScreenEffects {
    let mut effects = vec![];
    let lang = ctx.lang;

    let Some(session) = ctx.store.session.clone() else {
        // Ещё нет session.start (например, resume в процессе).
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            ui.weak(t(lang, Key::Loading));
        });
        return effects;
    };

    let (used, limit) = ctx.store.slot_usage();
    let pending = ctx.store.pending;

    ui.vertical_centered(|ui| {
        ui.add_space(20.0);
        ui.heading(t(lang, Key::CharactersTitle));
        ui.weak(format!(
            "{} · {}",
            session.server.name,
            tf(
                lang,
                Key::SlotsUsed,
                &[("used", &used.to_string()), ("limit", &limit.to_string())]
            )
        ));
        ui.add_space(12.0);

        if let Some(err) = &ctx.store.select_error {
            ui.colored_label(RED, err);
            ui.add_space(6.0);
        }
        if let Some(notice) = &ctx.store.notice {
            ui.weak(notice);
            ui.add_space(6.0);
        }

        for character in &session.characters {
            let mut enter = false;
            let mut delete = false;
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.heading(character.name.as_str());
                    for kind in &character.crime {
                        // kind — игровые данные сервера (marauder/…), отображаем как есть.
                        ui.colored_label(ORANGE, kind);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        delete = ui
                            .add_enabled(
                                pending.is_none(),
                                egui::Button::new(t(lang, Key::BtnDelete)),
                            )
                            .clicked();
                        enter = ui
                            .add_enabled(
                                pending.is_none(),
                                egui::Button::new(t(lang, Key::BtnEnter)),
                            )
                            .clicked();
                    });
                });
                ui.weak(format!(
                    "{} · {}",
                    character.summary.clone().unwrap_or_default(),
                    character.location
                ));
            });

            if enter {
                let cmd = CharSelect {
                    char_id: character.id.clone(),
                };
                match ctx.transport.send(&cmd) {
                    Ok(_) => ctx.store.on_select_sent(),
                    Err(e) => ctx.store.select_error = Some(format!("нет связи: {e}")),
                }
            }
            if delete {
                ctx.forms.delete_target = Some((character.id.clone(), character.name.clone()));
            }
            ui.add_space(8.0);
        }

        ui.add_space(10.0);
        let full = used as u32 >= limit;
        let create_label = if full {
            t(lang, Key::SlotsFull)
        } else {
            t(lang, Key::BtnCreate)
        };
        if ui
            .add_enabled(!full && pending.is_none(), egui::Button::new(create_label))
            .clicked()
        {
            ctx.forms.create.reset();
        }
        ui.add_space(8.0);
        if ui.button(t(lang, Key::BtnLogout)).clicked() {
            let _ = ctx.transport.send(&AuthLogout {});
            effects.extend(ctx.store.logout());
        }
    });

    // ── модалки ──
    if ctx.forms.create.open {
        create_modal(ctx, ui, &mut effects);
    }
    if let Some((id, name)) = ctx.forms.delete_target.clone() {
        delete_modal(ctx, ui, &mut effects, &id, &name);
    }
    effects
}

fn create_modal(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui, effects: &mut ScreenEffects) {
    let lang = ctx.lang;
    let mut close = false;
    egui::Window::new(t(lang, Key::CreateTitle))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            ui.label(t(lang, Key::ProfileLabel));
            let mut profile = ctx.forms.create.profile;
            for candidate in Profile::all() {
                if ui
                    .radio_value(&mut profile, candidate, t(lang, candidate.label_key()))
                    .clicked()
                {
                    ctx.forms.create.profile = candidate;
                }
            }
            ui.weak(t(lang, Key::ProfileHint));
            ui.add_space(8.0);

            ui.add(
                egui::TextEdit::singleline(&mut ctx.forms.create.name)
                    .hint_text(t(lang, Key::FieldCharName))
                    .desired_width(FORM_WIDTH),
            );
            if !ctx.forms.create.name.is_empty() && !ctx.forms.create.name_valid() {
                ui.weak(t(lang, Key::NameTooShort));
            }
            if let Some(err) = &ctx.store.create_error {
                ui.colored_label(RED, err);
            }
            ui.add_space(8.0);

            let pending_create = ctx.store.pending == Some(Act::Create);
            ui.horizontal(|ui| {
                if ui.button(t(lang, Key::Exit)).clicked() {
                    close = true;
                }
                let can_submit = ctx.forms.create.name_valid() && !pending_create;
                if ui
                    .add_enabled(can_submit, egui::Button::new(t(lang, Key::BtnCreate)))
                    .clicked()
                {
                    let cmd = CharCreate {
                        name: ctx.forms.create.name.trim().to_string(),
                        profile: ctx.forms.create.profile.value().to_string(),
                    };
                    match ctx.transport.send(&cmd) {
                        Ok(_) => {
                            ctx.store.on_create_sent();
                            ctx.forms.create.submitted = true;
                        }
                        Err(e) => ctx.store.create_error = Some(format!("нет связи: {e}")),
                    }
                }
            });

            // Успех (ack без ошибки) — окно закрывается, notice на экране.
            if ctx.forms.create.submitted
                && ctx.store.pending.is_none()
                && ctx.store.create_error.is_none()
            {
                close = true;
            }
        });

    if close {
        ctx.forms.create.open = false;
        ctx.forms.create.submitted = false;
        let _ = effects;
    }
}

fn delete_modal(
    ctx: &mut UiCtx<'_>,
    ui: &mut egui::Ui,
    effects: &mut ScreenEffects,
    id: &str,
    name: &str,
) {
    let lang = ctx.lang;
    let mut close = false;
    egui::Window::new(t(lang, Key::DeleteTitle))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            ui.colored_label(ORANGE, tf(lang, Key::DeleteConfirm, &[("name", name)]));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(t(lang, Key::Exit)).clicked() {
                    close = true;
                }
                let pending_delete = ctx.store.pending == Some(Act::Delete);
                if ui
                    .add_enabled(
                        !pending_delete,
                        egui::Button::new(egui::RichText::new(t(lang, Key::BtnDelete)).color(RED)),
                    )
                    .clicked()
                {
                    let cmd = CharDelete {
                        char_id: id.to_string(),
                    };
                    match ctx.transport.send(&cmd) {
                        Ok(_) => {
                            ctx.store.on_delete_sent();
                            close = true;
                        }
                        Err(e) => ctx.store.notice = Some(format!("нет связи: {e}")),
                    }
                }
            });
        });
    if close {
        ctx.forms.delete_target = None;
        let _ = effects;
    }
}

// ─── Заглушка игрового экрана (M3 развернёт панели по ФР ТЗ §7.1) ──────────

pub fn game(ctx: &mut UiCtx<'_>, ui: &mut egui::Ui) -> ScreenEffects {
    let mut effects = vec![];
    let lang = ctx.lang;

    let Some(snap) = ctx.store.snapshot.clone() else {
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            ui.weak(t(lang, Key::Loading));
        });
        return effects;
    };

    ui.vertical_centered(|ui| {
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.heading(snap.char.name.as_str());
            for status in &snap.char.crime.statuses {
                ui.colored_label(ORANGE, &status.kind);
            }
        });
        ui.weak(t(lang, Key::GamePlaceholder));
        ui.add_space(12.0);

        // Локация (ФР-03 приедет в M3 — пока имя и текст).
        ui.group(|ui| {
            ui.label(format!(
                "{}: {}",
                t(lang, Key::LocationLabel),
                snap.location.name
            ));
            ui.weak(snap.location.text.chars().take(200).collect::<String>());
        });
        ui.add_space(8.0);

        // Ресурсы (D4: mp — полноправный бар).
        let hp_frac = (snap.char.hp.cur as f32 / snap.char.hp.max.max(1) as f32).clamp(0.0, 1.0);
        ui.add(egui::ProgressBar::new(hp_frac).text(format!(
            "{}: {}/{}",
            t(lang, Key::HpLabel),
            snap.char.hp.cur,
            snap.char.hp.max
        )));
        let mp_frac = (snap.char.mp.cur as f32 / snap.char.mp.max.max(1) as f32).clamp(0.0, 1.0);
        ui.add(egui::ProgressBar::new(mp_frac).text(format!(
            "{}: {}/{}",
            t(lang, Key::MpLabel),
            snap.char.mp.cur,
            snap.char.mp.max
        )));
        ui.add_space(6.0);
        ui.weak(format!(
            "{}: {}",
            t(lang, Key::XpPoolLabel),
            snap.char.xp_pool
        ));
        ui.add_space(16.0);

        ui.horizontal(|ui| {
            if ui.button(t(lang, Key::BtnSwitchChar)).clicked() {
                // Hot-switch (ФР-02, DoD M2): чистый сброс снимка, без ре-логина.
                ctx.store.back_to_characters();
            }
            if ui.button(t(lang, Key::BtnLogout)).clicked() {
                let _ = ctx.transport.send(&AuthLogout {});
                effects.extend(ctx.store.logout());
            }
        });
    });
    effects
}

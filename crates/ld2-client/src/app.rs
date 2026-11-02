//! Главное окно приложения (eframe 0.37). M0 — шелл: заголовок, переключатель
//! языка, статус конфига. Панели мира/боя/чата — по ФР ТЗ §7.1 с M2.
//!
//! egui 0.37: корневой `Ui` приходит в `App::ui`; панели — `egui::Panel::top`,
//! центр — `egui::CentralPanel`.

use eframe::egui;

use crate::config::{self, Config, ConfigStore};
use crate::l10n::{self, Key, Lang};

pub struct App {
    lang: Lang,
    config: Config,
    store: ConfigStore,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, config: Config, store: ConfigStore) -> Self {
        // egui использует встроенный (embedded) шрифт Ubuntu-Light с кириллицей
        // в epaint — не полагаемся на системные шрифты (ТЗ §7.5).
        Self {
            lang: Lang::from_code(&config.lang),
            config,
            store,
        }
    }

    fn toggle_lang(&mut self) {
        self.lang = self.lang.toggle();
        self.config.lang = self.lang.code().to_string();
        if let Err(e) = self.store.save(&self.config) {
            tracing::error!("не удалось сохранить конфиг: {e:#}");
        } else {
            tracing::info!("язык переключён на {}", self.lang.code());
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(l10n::t(self.lang, Key::AppTitle));
                ui.separator();
                ui.weak(l10n::t(self.lang, Key::Subtitle));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(l10n::t(self.lang, Key::Exit)).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    let label = format!(
                        "{}: {}",
                        l10n::t(self.lang, Key::Language),
                        self.lang.code().to_uppercase()
                    );
                    if ui.button(label).on_hover_text("RU / EN").clicked() {
                        self.toggle_lang();
                    }
                });
            });
        });
    }

    fn body(&self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(l10n::t(self.lang, Key::M0Title));
            ui.add_space(8.0);
            ui.label(format!(
                "{}: {}",
                l10n::t(self.lang, Key::Host),
                self.config.host
            ));
            ui.label(format!(
                "{}: {}",
                l10n::t(self.lang, Key::Version),
                env!("CARGO_PKG_VERSION")
            ));
            if let Ok(dirs) = config::project_dirs() {
                ui.label(format!(
                    "{}: {}",
                    l10n::t(self.lang, Key::ConfigPath),
                    dirs.config_dir().display()
                ));
                ui.label(format!(
                    "{}: {}",
                    l10n::t(self.lang, Key::DataPath),
                    dirs.data_dir().display()
                ));
            }
            ui.add_space(12.0);
            ui.weak(l10n::t(self.lang, Key::M0Hint));
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.header(ui);
        self.body(ui);
    }
}

//! ld2-editor — редактор контента ld2 (gpui + Ely).
//!
//! Файлы `content/*.json` — источник правды (git); редактор пишет их
//! атомарно, с валидацией и картой мира. План: `plans/PLAN-M27-EDITOR.md`.
//!
//! E0 (текущий): каркас крейта и окно. E1+: общий `ld2-content`,
//! формы/списки, карта-полотно.

mod app;
mod map;
mod project;
mod ui_tokens;

const HELP: &str = "\
ld2-editor — редактор контента ld2 (gpui + Ely)

Использование:
  ld2-editor              окно редактора (проект: CWD или --dir PATH)
  ld2-editor --check [DIR] проверка контента без GUI (валидация/версия;
                          код выхода 1 — есть фатальные ошибки)
  ld2-editor --normalize-content [DIR]
                          привести JSON-файлы каталога к каноническому
                          pretty-формату (2 пробела + \n) и выйти;
                          DIR по умолчанию — content
  ld2-editor --version    версия и выход
  ld2-editor --help       эта справка

Источник правды — файлы content/*.json в git; сервер применяет изменения
через `ld2-server seed` / hot reload (poll, SIGHUP, admin.content.reload).
";

use ely::Assets;
use gpui::{px, size, App, AppContext, Bounds, QuitMode, WindowBounds, WindowOptions};
use gpui_platform::application;

fn main() -> anyhow::Result<()> {
    // CLI-режимы без GUI (CI-смоук/справка) — до инициализации gpui.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("ld2-editor {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    // CLI-проверка контента без GUI (CI/скрипты деплоя): валидация и
    // версия; код выхода 1 при фатальных ошибках.
    if let Some(pos) = args.iter().position(|a| a == "--check") {
        let dir = args
            .get(pos + 1)
            .cloned()
            .unwrap_or_else(|| ".".to_string());
        let project = match crate::project::EditorProject::open(&dir) {
            Ok(project) => project,
            Err(err) => {
                eprintln!("ошибка проекта: {err}");
                std::process::exit(2);
            }
        };
        let report = project.report();
        println!(
            "контент: {} файлов, версия {}, ошибок {}, предупреждений {}",
            ld2_content::ContentFile::ALL.len(),
            project.version(),
            report.errors.len(),
            report.warnings.len()
        );
        for error in &report.errors {
            eprintln!("✗ {error}");
        }
        for warning in &report.warnings {
            eprintln!("⚠ {warning}");
        }
        if report.is_valid() {
            return Ok(());
        }
        std::process::exit(1);
    }

    // CLI-нормализация контента (E2): один раз привести файлы каталога к
    // каноническому виду, чтобы сохранение из редактора давало diff
    // только по правкам. Семантика контента не меняется (версия — по
    // типизированному дереву, а не по тексту файла).
    if let Some(pos) = args.iter().position(|a| a == "--normalize-content") {
        let dir = args
            .get(pos + 1)
            .cloned()
            .unwrap_or_else(|| "content".to_string());
        let dir = std::path::Path::new(&dir);
        let raw = ld2_content::RawContent::load(dir)?;
        let files = raw.save(dir, &ld2_content::ContentFile::ALL)?;
        println!("нормализовано файлов: {files} ({})", dir.display());
        return Ok(());
    }

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    tracing::info!("ld2-editor v{} — старт", env!("CARGO_PKG_VERSION"));

    application()
        .with_assets(Assets)
        // macOS: закрытие окна завершает процесс (как в клиенте, M11).
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx: &mut App| {
            // Ely: шрифты/тема/Tab-биндинги — «call once, first».
            ely::init(cx).expect("Ely init");

            let win_size = cx
                .primary_display()
                .map(|d| {
                    let b = d.visible_bounds();
                    size(b.size.width * 0.9, b.size.height * 0.9)
                })
                .unwrap_or(size(px(1280.0), px(800.0)));
            let bounds = Bounds::centered(None, win_size, cx);
            let editor = cx.new(app::EditorApp::new);
            // Горячие клавиши редактора (Ctrl/Cmd+S/Z/Y/R) — через
            // наблюдатель keystroke, как навигация клиента (main.rs).
            let keys = editor.clone();
            cx.observe_keystrokes(move |event, window, cx| {
                keys.update(cx, |editor, cx| {
                    editor.handle_keystroke(&event.keystroke, window, cx);
                });
            })
            .detach();
            cx.open_window(
                WindowOptions {
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("ld2-editor".into()),
                        ..Default::default()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(1024.0), px(700.0))),
                    ..Default::default()
                },
                move |_, _| editor.clone(),
            )
            .expect("окно");
            cx.activate(true);
        });
    Ok(())
}

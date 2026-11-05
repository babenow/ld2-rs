//! CLI реального сервера (PLAN-M12.1).
//!
//! Все настройки — в `server.toml` (D25): путь задаётся `--config` или
//! переменной окружения `LD2_SERVER_CONFIG` (только путь к файлу, не значение).

use std::path::PathBuf;
use std::process::ExitCode;

use ld2_server::config::{Config, ConfigError};
use tracing_subscriber::EnvFilter;

const HELP: &str = "\
ld2-server — реальный игровой сервер ld2 (docs/protocol.md, PostgreSQL)

Использование:
  ld2-server [--config PATH] [run]     запуск (значение по умолчанию)
  ld2-server [--config PATH] check     только проверить конфиг и выйти
  ld2-server [--config PATH] migrate   применить миграции и выйти
  ld2-server --help                    эта справка

  --config PATH   путь к server.toml (по умолчанию ./server.toml;
                  альтернатива — переменная LD2_SERVER_CONFIG:
                  только путь к файлу, значение настройки — никогда)

Все настройки сервера — в файле конфига (D25); шаблон: server.example.toml.
";

#[derive(Clone, Copy)]
enum Command {
    Run,
    Check,
    Migrate,
}

enum Args {
    Help,
    Parsed { command: Command, config: PathBuf },
}

fn parse_args() -> Result<Args, String> {
    let mut command_name: Option<String> = None;
    let mut config: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Args::Help),
            "--config" => {
                let value = args.next().ok_or("--config: ожидается путь к файлу")?;
                config = Some(PathBuf::from(value));
            }
            "run" | "check" | "migrate" => {
                if command_name.is_some() {
                    return Err(format!("подкоманда указана дважды: {arg}"));
                }
                command_name = Some(arg);
            }
            other => return Err(format!("неизвестный аргумент {other} (см. --help)")),
        }
    }

    let command = match command_name.as_deref() {
        None | Some("run") => Command::Run,
        Some("check") => Command::Check,
        Some("migrate") => Command::Migrate,
        Some(other) => unreachable!("подкоманда проверена в цикле: {other}"),
    };
    let config = config
        .or_else(|| std::env::var_os("LD2_SERVER_CONFIG").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("server.toml"));
    Ok(Args::Parsed { command, config })
}

enum AppError {
    Usage(String),
    Config(ConfigError),
    Runtime(anyhow::Error),
}

impl From<String> for AppError {
    fn from(msg: String) -> Self {
        AppError::Usage(msg)
    }
}

impl From<ConfigError> for AppError {
    fn from(err: ConfigError) -> Self {
        AppError::Config(err)
    }
}

impl From<ld2_server::db::DbError> for AppError {
    fn from(err: ld2_server::db::DbError) -> Self {
        AppError::Runtime(anyhow::Error::new(err))
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Runtime(anyhow::Error::new(err))
    }
}

async fn run() -> Result<(), AppError> {
    let (command, config_path) = match parse_args()? {
        Args::Help => {
            print!("{HELP}");
            return Ok(());
        }
        Args::Parsed { command, config } => (command, config),
    };

    // Fail-fast: без валидного конфига не запускаемся (D25), код выхода 2.
    let cfg = Config::load(&config_path)?;

    if matches!(command, Command::Check) {
        println!("конфиг {} валиден", config_path.display());
        return Ok(());
    }

    let filter =
        EnvFilter::try_new(&cfg.server.log_level).expect("log_level прошёл валидацию конфига");
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let pool = ld2_server::db::connect(&cfg.database).await?;
    tracing::info!(
        "миграции применены, БД готова (пул: {} соед.)",
        cfg.database.max_connections
    );

    if matches!(command, Command::Migrate) {
        pool.close().await;
        return Ok(());
    }

    let listener = tokio::net::TcpListener::bind(&cfg.server.bind).await?;
    let addr = listener.local_addr()?;
    tracing::info!(
        "сервер «{}» ({}) запускается: {addr}, ws → ws://{addr}/ws, \
         healthz → http://{addr}/healthz, контент {:?}, тик {} Гц, автосейв {} с",
        cfg.server.name,
        cfg.server.region,
        cfg.world.content_dir,
        cfg.world.tick_hz,
        cfg.world.autosave_interval_s
    );
    let state = std::sync::Arc::new(ld2_server::state::AppState::new(cfg, pool));
    ld2_server::app::serve(listener, state).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(AppError::Usage(msg)) => {
            eprintln!("ошибка аргументов: {msg}\n\n{HELP}");
            ExitCode::from(2)
        }
        Err(AppError::Config(err)) => {
            eprintln!("{err}");
            ExitCode::from(2)
        }
        Err(AppError::Runtime(err)) => {
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

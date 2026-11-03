//! CLI мок-сервера (PLAN-M1 T3).

use ld2_mock_server::{serve, Chaos, MockOptions};
use tracing_subscriber::EnvFilter;

const HELP: &str = "\
ld2-mock-server — детерминированный мок-сервер ld2 (docs/protocol.md)

Использование:
  ld2-mock-server [--bind ADDR] [--scenario NAME] [--seed N] [--chaos-every N]
                  [--char-slots N]

  --bind           адрес (по умолчанию 127.0.0.1:24817)
  --scenario       hello | snapshot | chaos | all (по умолчанию all)
                     hello    — auth/demo + session.start (всегда доступен)
                     snapshot — char.select + снапшот (всегда доступен)
                     chaos    — включает случайные обрывы WS
                     all      — все сценарии без chaos
  --seed           seed chaos-последовательности (по умолчанию 42)
  --chaos-every    1/N — шанс обрыва после сообщения (по умолчанию 5)
  --char-slots     лимит слотов персонажей в session.start (по умолчанию 8);
                   DoD M2: меняется конфигом, без правки кода клиента
";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let mut bind = "127.0.0.1:24817".to_string();
    let mut scenario = "all".to_string();
    let mut seed: u64 = 42;
    let mut chaos_every: u32 = 5;
    let mut char_slots: u32 = ld2_mock_server::fixtures::CHAR_SLOT_LIMIT;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let next = |i: &mut usize| -> Option<String> {
            *i += 1;
            args.get(*i).cloned()
        };
        match flag {
            "--bind" => {
                bind = next(&mut i).ok_or_else(|| anyhow::anyhow!("нет значения --bind"))?
            }
            "--scenario" => {
                scenario = next(&mut i).ok_or_else(|| anyhow::anyhow!("нет значения --scenario"))?
            }
            "--seed" => {
                let v = next(&mut i).ok_or_else(|| anyhow::anyhow!("нет значения --seed"))?;
                seed = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--seed: число ожидалось"))?;
            }
            "--chaos-every" => {
                let v =
                    next(&mut i).ok_or_else(|| anyhow::anyhow!("нет значения --chaos-every"))?;
                chaos_every = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--chaos-every: число ожидалось"))?;
            }
            "--char-slots" => {
                let v = next(&mut i).ok_or_else(|| anyhow::anyhow!("нет значения --char-slots"))?;
                char_slots = v
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--char-slots: число ожидалось"))?;
                if char_slots == 0 {
                    anyhow::bail!("--char-slots: должен быть > 0");
                }
            }
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            other => anyhow::bail!("неизвестный аргумент {other} (см. --help)"),
        }
        i += 1;
    }

    if !["hello", "snapshot", "chaos", "all"].contains(&scenario.as_str()) {
        anyhow::bail!("неизвестный сценарий {scenario} (см. --help)");
    }

    let chaos_enabled = scenario == "chaos";
    serve(MockOptions {
        bind,
        chaos: Chaos::new(chaos_enabled, chaos_every, seed),
        scenario,
        char_slot_limit: char_slots,
    })
    .await
}

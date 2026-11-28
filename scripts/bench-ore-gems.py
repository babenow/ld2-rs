#!/usr/bin/env python3
"""Бенчмарк рудных жил: сколько камней падает и как часто (M39).

Считает по реальному контенту (`content/locations.json`) и дефолтному
конфигу (`crates/ld2-server/server.example.toml`):

* шанс камня за успешную добычу и за попытку (с учётом 5 % провала);
* ожидаемое число камней с полной жилы и распределение P(0..N);
* «1 камень в N попыток» и камни/час на одной жиле (добыча + респавн);
* эффект «Кирки мастера» (×2) и «Лампы шахтёра» (+2 п.п.);
* проверку формул Монте-Карло (фиксированный seed).

Запуск:
    python3 scripts/bench-ore-gems.py
    python3 scripts/bench-ore-gems.py --write

`--write` обновляет `docs/benchmarks/ore-gems.md`.
"""

from __future__ import annotations

import argparse
import json
import math
import random
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTENT = ROOT / "content"
CONFIG = ROOT / "crates/ld2-server/server.example.toml"
REPORT = ROOT / "docs/benchmarks/ore-gems.md"

ORE_ORDER = [
    "ore_copper",
    "ore_tin",
    "ore_iron",
    "ore_bronze",
    "ore_silver",
    "ore_gold",
    "ore_platinum",
    "ore_adamantite",
    "ore_mithril",
]

TOOLS = [
    ("Без инструментов", 1.0, 0.0),
    ("Кирка мастера", None, 0.0),  # множитель из конфига
    ("Лампа шахтёра", 1.0, None),  # прибавка из конфига
    ("Кирка + лампа", None, None),
]


def load_professions() -> dict:
    with CONFIG.open("rb") as fh:
        raw = tomllib.load(fh)
    return raw["gameplay"]["professions"]


def action_chance(cfg: dict, required: int, level: int) -> float:
    base = min(max(float(cfg["mining_chance_base"]), 0.0), 1.0)
    minimum = min(max(float(cfg["mining_chance_min"]), 0.0), base)
    diff = max(0, required - level)
    return min(
        max(base - diff * max(float(cfg["mining_chance_per_level"]), 0.0), minimum),
        base,
    )


def gem_chance(base: float, mult: float, add: float) -> float:
    return min(max(base * mult + add, 0.0), 1.0)


def gather_seconds(required: int, level: int) -> int:
    gap = min(max(0, required - level), 24)
    return 2 + round(gap / 24 * 4)


def binom_pmf(n: int, p: float, k: int) -> float:
    return math.comb(n, k) * (p**k) * ((1.0 - p) ** (n - k))


def fmt_pct(value: float, digits: int = 2) -> str:
    return f"{value * 100:.{digits}f}%"


def fmt_num(value: float, digits: int = 3) -> str:
    return f"{value:.{digits}f}"


def load_veins() -> dict[str, dict]:
    """По одной (самой ёмкой) жиле на руду; остальные — в `extra_max`."""
    with (CONTENT / "locations.json").open(encoding="utf-8") as fh:
        locations = json.load(fh)["locations"]
    with (CONTENT / "items.json").open(encoding="utf-8") as fh:
        items = {item["id"]: item for item in json.load(fh)["items"]}

    veins: dict[str, dict] = {}
    for loc in locations:
        for entity in loc.get("entities", []):
            gems = entity.get("gems")
            ore = entity.get("resource")
            if not gems or ore not in ORE_ORDER:
                continue
            max_stock = max(int(entity.get("max", 1)), 1)
            entry = {
                "ore": ore,
                "name": items.get(ore, {}).get("name", ore),
                "required": int(entity.get("required_level", 1)),
                "max": max_stock,
                "chance": float(gems.get("chance", 0.0)),
                "respawn_min": float(entity.get("respawn", {}).get("min_s", 60)),
                "respawn_max": float(entity.get("respawn", {}).get("max_s", 120)),
                "location": loc.get("id", ""),
            }
            current = veins.get(ore)
            if current is None:
                veins[ore] = entry
            else:
                current.setdefault("extra_max", []).append(max_stock)
                # Для отчёта — самая ёмкая жила; шанс у руды одинаков.
                if max_stock > current["max"]:
                    current.update(entry)
    return veins


def row_metrics(vein: dict, cfg: dict, mult: float, add: float) -> dict:
    required = vein["required"]
    level = required  # «целевой» уровень: гейт пройден, добыча 2 с
    p_gem = gem_chance(vein["chance"], mult, add)
    p_attempt = action_chance(cfg, required, level) * p_gem
    n = vein["max"]
    expected = n * p_attempt
    p0 = (1.0 - p_attempt) ** n
    dist = [binom_pmf(n, p_attempt, k) for k in range(n + 1)]
    gather_s = gather_seconds(required, level)
    respawn_s = (vein["respawn_min"] + vein["respawn_max"]) / 2.0
    cycle_s = n * gather_s + respawn_s
    return {
        "p_gem": p_gem,
        "p_attempt": p_attempt,
        "expected": expected,
        "p0": p0,
        "p_at_least_one": 1.0 - p0,
        "attempts_per_gem": (1.0 / p_attempt) if p_attempt > 0 else math.inf,
        "gems_per_hour": expected * 3600.0 / cycle_s,
        "dist": dist,
        "cycle_s": cycle_s,
    }


def simulate(vein: dict, cfg: dict, mult: float, add: float, cycles: int, rng: random.Random) -> tuple[float, float]:
    metrics = row_metrics(vein, cfg, mult, add)
    p = metrics["p_attempt"]
    n = vein["max"]
    total = 0
    empty = 0
    for _ in range(cycles):
        found = 0
        for _ in range(n):
            if rng.random() < p:
                found += 1
        total += found
        if found == 0:
            empty += 1
    return total / cycles, empty / cycles


def render(cfg: dict, veins: dict[str, dict], cycles: int) -> str:
    master_mult = float(cfg["mining_master_gem_mult"])
    lamp_add = float(cfg["mining_lamp_gem_bonus"])
    tools = [
        (name, master_mult if mult is None else mult, lamp_add if add is None else add)
        for name, mult, add in TOOLS
    ]

    lines: list[str] = []
    lines.append("# Бенчмарк рудных жил — камни (M39)")
    lines.append("")
    lines.append(
        "Сгенерировано `scripts/bench-ore-gems.py` по `content/locations.json` "
        "и `server.example.toml`."
    )
    lines.append("")
    lines.append("## Допущения")
    lines.append("")
    lines.append(
        "- уровень профессии = требуемому уровню жилы: гейт пройден, "
        "добыча 2 с, шанс успеха 95 %;"
    )
    lines.append(
        "- провал добычи (5 %) тоже списывает 1 запас жилы, но камня не даёт;"
    )
    lines.append(
        "- одна попытка = одна единица запаса; камень — 1 шт. за успешный бросок;"
    )
    lines.append(
        "- «Камней/час» — одна жила: полный запас × 2 с + средний респавн; "
        "без переходов между жилами;"
    )
    lines.append(
        "- для руды взята самая ёмкая жила (у олова в лесу есть жила с запасом 2);"
    )
    lines.append(
        f"- инструменты: кирка ×{master_mult:g}, лампа +{lamp_add * 100:g} п.п. "
        "(`mining_master_gem_mult` / `mining_lamp_gem_bonus`)."
    )
    lines.append("")

    # ── 1. База без инструментов
    lines.append("## 1. Без инструментов")
    lines.append("")
    lines.append(
        "| Руда | Ур. | Запас жилы | Шанс/успех | Шанс/попытка | "
        "1 камень в N попыток | Камней/жила | ≥1 камень/жила | Камней/час |"
    )
    lines.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|")
    for ore in ORE_ORDER:
        vein = veins.get(ore)
        if vein is None:
            continue
        m = row_metrics(vein, cfg, 1.0, 0.0)
        lines.append(
            f"| {vein['name']} | {vein['required']} | {vein['max']} | "
            f"{fmt_pct(m['p_gem'])} | {fmt_pct(m['p_attempt'])} | "
            f"{m['attempts_per_gem']:.1f} | {fmt_num(m['expected'])} | "
            f"{fmt_pct(m['p_at_least_one'], 1)} | {fmt_num(m['gems_per_hour'], 2)} |"
        )
    lines.append("")

    # ── 2. Эффект инструментов
    lines.append("## 2. Эффект инструментов")
    lines.append("")
    lines.append("Шанс камня за успешную добычу / ожидаемое число камней с полной жилы.")
    lines.append("")
    lines.append("| Руда | " + " | ".join(name for name, _, _ in tools) + " |")
    lines.append("|---|" + "---|" * len(tools))
    for ore in ORE_ORDER:
        vein = veins.get(ore)
        if vein is None:
            continue
        cells = []
        for _, mult, add in tools:
            m = row_metrics(vein, cfg, mult, add)
            cells.append(f"{fmt_pct(m['p_gem'])} / {fmt_num(m['expected'])}")
        lines.append(f"| {vein['name']} | " + " | ".join(cells) + " |")
    lines.append("")

    # ── 3. Распределение
    lines.append("## 3. Распределение камней с жилы")
    lines.append("")
    lines.append("Вероятности P(k) для обычной добычи и с киркой+лампой; N — запас жилы.")
    lines.append("")
    lines.append("| Руда | N | Без инструментов P(0..N) | Кирка + лампа P(0..N) |")
    lines.append("|---|---:|---|---|")
    for ore in ORE_ORDER:
        vein = veins.get(ore)
        if vein is None:
            continue
        base = row_metrics(vein, cfg, 1.0, 0.0)["dist"]
        both = row_metrics(vein, cfg, master_mult, lamp_add)["dist"]
        base_s = " / ".join(fmt_pct(x, 2) for x in base)
        both_s = " / ".join(fmt_pct(x, 2) for x in both)
        lines.append(f"| {vein['name']} | {vein['max']} | {base_s} | {both_s} |")
    lines.append("")

    # ── 4. Монте-Карло
    rng = random.Random(20261128)
    max_mean_delta = 0.0
    max_p0_delta = 0.0
    for ore in ORE_ORDER:
        vein = veins.get(ore)
        if vein is None:
            continue
        for _, mult, add in tools:
            exact = row_metrics(vein, cfg, mult, add)
            mc_mean, mc_p0 = simulate(vein, cfg, mult, add, cycles, rng)
            max_mean_delta = max(max_mean_delta, abs(mc_mean - exact["expected"]))
            max_p0_delta = max(max_p0_delta, abs(mc_p0 - exact["p0"]))
    lines.append("## 4. Проверка Монте-Карло")
    lines.append("")
    lines.append(
        f"{cycles} циклов на каждую комбинацию (руда × инструменты), "
        "фиксированный seed `20261128`."
    )
    lines.append("")
    lines.append(f"- максимальное расхождение среднего: **{max_mean_delta:.4f}** камня;")
    lines.append(f"- максимальное расхождение P(0): **{max_p0_delta:.4f}**.")
    lines.append("")
    lines.append("Точные значения выше — биномиальное распределение; Монте-Карло только подтверждает формулы.")
    lines.append("")
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description="Бенчмарк камней из рудных жил")
    parser.add_argument("--write", action="store_true", help="записать docs/benchmarks/ore-gems.md")
    parser.add_argument("--cycles", type=int, default=200_000, help="циклов Монте-Карло на комбинацию")
    args = parser.parse_args()

    cfg = load_professions()
    veins = load_veins()
    text = render(cfg, veins, args.cycles)
    if args.write:
        REPORT.parent.mkdir(parents=True, exist_ok=True)
        REPORT.write_text(text + "\n", encoding="utf-8")
        print(f"записано: {REPORT.relative_to(ROOT)}")
    else:
        print(text)


if __name__ == "__main__":
    main()

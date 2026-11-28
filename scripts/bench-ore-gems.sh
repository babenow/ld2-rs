#!/usr/bin/env bash
# Бенчмарк рудных жил: сколько камней падает и как часто (M39).
#
# Считает по реальному контенту (content/locations.json) и дефолтному
# конфигу (crates/ld2-server/server.example.toml). Отчёт — Markdown,
# по умолчанию пишется в docs/benchmarks/ore-gems.md (файл в .gitignore).
#
# Требования: bash, jq, awk. Запуск:
#   scripts/bench-ore-gems.sh
#   scripts/bench-ore-gems.sh --stdout   # только печать, без записи
set -euo pipefail

cd "$(dirname "$0")/.."

OUT="docs/benchmarks/ore-gems.md"
CONFIG="crates/ld2-server/server.example.toml"
LOCATIONS="content/locations.json"
ITEMS="content/items.json"
STDOUT=0

for arg in "$@"; do
    case "$arg" in
        --stdout) STDOUT=1 ;;
        -h|--help)
            sed -n '2,12p' "$0"
            exit 0
            ;;
        *)
            echo "неизвестный аргумент: $arg" >&2
            exit 2
            ;;
    esac
done

command -v jq >/dev/null 2>&1 || {
    echo "нужен jq (brew install jq / apt install jq)" >&2
    exit 1
}

read_cfg() {
    local key="$1"
    awk -v key="$key" '
        /^\[gameplay\.professions\]/ { inside = 1; next }
        /^\[/ { inside = 0 }
        inside && $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
            line = $0
            sub(/#.*/, "", line)
            split(line, parts, "=")
            value = parts[2]
            gsub(/[[:space:]]/, "", value)
            print value
            exit
        }
    ' "$CONFIG"
}

BASE=$(read_cfg mining_chance_base)
PER_LEVEL=$(read_cfg mining_chance_per_level)
MIN=$(read_cfg mining_chance_min)
MASTER_MULT=$(read_cfg mining_master_gem_mult)
LAMP_ADD=$(read_cfg mining_lamp_gem_bonus)

for value in "$BASE" "$PER_LEVEL" "$MIN" "$MASTER_MULT" "$LAMP_ADD"; do
    [[ -n "$value" ]] || { echo "не найдены настройки [gameplay.professions]" >&2; exit 1; }
done

NAMES=$(jq -r '.items[] | select(.id | startswith("ore_")) | [.id, .name] | @tsv' "$ITEMS")
VEINS=$(jq -r '
    .locations[] as $loc
    | $loc.entities[]?
    | select(.gems != null and .resource != null)
    | [
        .resource,
        (.required_level // 1),
        (.max // 1),
        .gems.chance,
        (.respawn.min_s // 60),
        (.respawn.max_s // 120),
        ($loc.id // "")
      ] | @tsv
' "$LOCATIONS")

render() {
    awk -F '\t' \
        -v base="$BASE" -v per_level="$PER_LEVEL" -v min_chance="$MIN" \
        -v master_mult="$MASTER_MULT" -v lamp_add="$LAMP_ADD" '
    function min(a, b) { return a < b ? a : b }
    function max(a, b) { return a > b ? a : b }
    function comb(n, k,    i, r) {
        r = 1
        for (i = 1; i <= k; i++) r = r * (n - k + i) / i
        return r
    }
    function pmf(n, p, k) {
        return comb(n, k) * (p ^ k) * ((1 - p) ^ (n - k))
    }
    function pct(p, digits) { return sprintf("%." digits "f%%", p * 100) }
    function num(x, digits) { return sprintf("%." digits "f", x) }
    function action_chance(required,    diff) {
        diff = max(0, required - required) # уровень = требуемому
        return min(max(base - diff * per_level, min_chance), base)
    }
    function gem_chance(ch, mult, add) { return min(max(ch * mult + add, 0), 1) }
    function metrics(ch, mult, add, required, stock, rmin, rmax,    pg, pa, expected, p0, cycle) {
        pg = gem_chance(ch, mult, add)
        pa = action_chance(required) * pg
        expected = stock * pa
        p0 = (1 - pa) ^ stock
        cycle = stock * 2 + (rmin + rmax) / 2
        return sprintf("%s\t%s\t%s\t%s\t%s\t%s", pg, pa, expected, p0, cycle, expected * 3600 / cycle)
    }
    BEGIN {
        split("ore_copper ore_tin ore_iron ore_bronze ore_silver ore_gold ore_platinum ore_adamantite ore_mithril", order, " ")
        print "# Бенчмарк рудных жил — камни (M39)"
        print ""
        print "Сгенерировано `scripts/bench-ore-gems.sh` / `bench-ore-gems.bat` по `content/locations.json` и `server.example.toml`."
        print ""
        print "## Допущения"
        print ""
        print "- уровень профессии = требуемому уровню жилы: гейт пройден, добыча 2 с, шанс успеха 95 %;"
        print "- провал добычи (5 %) тоже списывает 1 запас жилы, но камня не даёт;"
        print "- одна попытка = одна единица запаса; камень — 1 шт. за успешный бросок;"
        print "- «Камней/час» — одна жила: полный запас × 2 с + средний респавн; без переходов между жилами;"
        print "- для руды взята самая ёмкая жила (у олова в лесу есть жила с запасом 2);"
        printf("- инструменты: кирка ×%g, лампа +%g п.п. (`mining_master_gem_mult` / `mining_lamp_gem_bonus`).\n", master_mult, lamp_add * 100)
        print ""
        print "## 1. Без инструментов"
        print ""
        print "| Руда | Ур. | Запас жилы | Шанс/успех | Шанс/попытка | 1 камень в N попыток | Камней/жила | ≥1 камень/жила | Камней/час |"
        print "|---|---:|---:|---:|---:|---:|---:|---:|---:|"
    }
    NR == FNR {
        name[$1] = $2
        next
    }
    {
        ore = $1
        required = $2 + 0
        stock = $3 + 0
        chance = $4 + 0
        rmin = $5 + 0
        rmax = $6 + 0
        if (!(ore in best_stock) || stock > best_stock[ore]) {
            best_stock[ore] = stock
            best_required[ore] = required
            best_chance[ore] = chance
            best_rmin[ore] = rmin
            best_rmax[ore] = rmax
        }
    }
    END {
        for (i = 1; i <= 9; i++) {
            ore = order[i]
            if (!(ore in best_stock)) continue
            m = metrics(best_chance[ore], 1, 0, best_required[ore], best_stock[ore], best_rmin[ore], best_rmax[ore])
            split(m, v, "\t")
            printf("| %s | %d | %d | %s | %s | %.1f | %s | %s | %s |\n",
                name[ore], best_required[ore], best_stock[ore],
                pct(v[1], 2), pct(v[2], 2), 1 / v[2], num(v[3], 3), pct(1 - v[4], 1), num(v[6], 2))
        }
        print ""
        print "## 2. Эффект инструментов"
        print ""
        print "Шанс камня за успешную добычу / ожидаемое число камней с полной жилы."
        print ""
        print "| Руда | Без инструментов | Кирка мастера | Лампа шахтёра | Кирка + лампа |"
        print "|---|---|---|---|---|"
        for (i = 1; i <= 9; i++) {
            ore = order[i]
            if (!(ore in best_stock)) continue
            line = "| " name[ore] " |"
            split("1 0\n" master_mult " 0\n" "1 " lamp_add "\n" master_mult " " lamp_add, configs, "\n")
            for (j = 1; j <= 4; j++) {
                split(configs[j], ma, " ")
                m = metrics(best_chance[ore], ma[1], ma[2], best_required[ore], best_stock[ore], best_rmin[ore], best_rmax[ore])
                split(m, v, "\t")
                line = line " " pct(v[1], 2) " / " num(v[3], 3) " |"
            }
            print line
        }
        print ""
        print "## 3. Распределение камней с жилы"
        print ""
        print "Вероятности P(k) для обычной добычи и с киркой+лампой; N — запас жилы."
        print ""
        print "| Руда | N | Без инструментов P(0..N) | Кирка + лампа P(0..N) |"
        print "|---|---:|---|---|"
        for (i = 1; i <= 9; i++) {
            ore = order[i]
            if (!(ore in best_stock)) continue
            n = best_stock[ore]
            m = metrics(best_chance[ore], 1, 0, best_required[ore], n, best_rmin[ore], best_rmax[ore])
            split(m, v, "\t")
            plain = ""
            for (k = 0; k <= n; k++) plain = plain (k ? " / " : "") pct(pmf(n, v[2], k), 2)
            m = metrics(best_chance[ore], master_mult, lamp_add, best_required[ore], n, best_rmin[ore], best_rmax[ore])
            split(m, v, "\t")
            both = ""
            for (k = 0; k <= n; k++) both = both (k ? " / " : "") pct(pmf(n, v[2], k), 2)
            printf("| %s | %d | %s | %s |\n", name[ore], n, plain, both)
        }
        print ""
        print "Точные значения — биномиальное распределение; камень выпадает только при успешной добыче."
    }
    ' <(printf '%s\n' "$NAMES") <(printf '%s\n' "$VEINS")
}

if [[ "$STDOUT" -eq 1 ]]; then
    render
else
    mkdir -p "$(dirname "$OUT")"
    tmp=$(mktemp)
    render >"$tmp"
    mv "$tmp" "$OUT"
    chmod 644 "$OUT"
    echo "записано: $OUT"
fi

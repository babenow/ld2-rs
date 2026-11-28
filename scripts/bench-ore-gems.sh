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
        print "### Как читать таблицу 1"
        print ""
        print "- **Шанс/успех** — шанс камня в одном успешном сборе (поле `gems.chance` у жилы)."
        print "- **Шанс/попытка** — с учётом 5 % провала добычи: `0.95 × шанс/успех`. Провал тоже списывает 1 запас, поэтому это вероятность на единицу запаса."
        print "- **1 камень в N попыток** — `1 / шанс/попытка`: сколько в среднем единиц запаса нужно пройти до первого камня."
        print "- **Камней/жила** — ожидание для полной жилы: `запас × шанс/попытка`."
        print "- **≥1 камень/жила** — `1 − P(0)`: шанс, что за полный запас выпадет хотя бы один камень."
        print "- **Камней/час** — одна жила без переходов: `камней/жила × 3600 / (запас × 2 с + средний респавн)`."
        print ""
        print "Пример для меди: 3 единицы запаса × 1.1875% = 0.036 камня с жилы; один камень в среднем раз в 84.2 попытки; P(0) = 96.5%; цикл 3 × 2 с + 90 с = 96 с → 1.34 камня/час."
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
        print "### Как читать таблицу 2"
        print ""
        print "В каждой ячейке — **шанс камня за успешную добычу / ожидаемое число камней с полной жилы**."
        print ""
        print "- **Без инструментов** — базовый шанс жилы."
        print "- **Кирка мастера** — `min(1, base × mining_master_gem_mult)`; по умолчанию ×2."
        print "- **Лампа шахтёра** — `min(1, base + mining_lamp_gem_bonus)`; по умолчанию +2 п.п."
        print "- **Кирка + лампа** — `min(1, base × mult + add)`. Ожидание уже включает 95 % успеха и полный запас жилы."
        print ""
        print "Сравнивать колонки стоит по обоим числам: шанс показывает «частоту события», а камней/жила — сколько в среднем получит игрок за один цикл."
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
        print "### Как читать таблицу 3"
        print ""
        print "Столбцы `P(0..N)` — вероятности получить ровно `k` камней за полный запас жилы, где `N` — запас. Считается по биномиальному распределению:"
        print ""
        print "`P(k) = C(N,k) × p^k × (1−p)^(N−k)`, где `p` — шанс/попытка из таблицы 1."
        print ""
        print "- `P(0)` — жила не дала ни одного камня."
        print "- `P(1)` — ровно один камень, `P(2)` — ровно два и т. д."
        print "- Сумма всех `P(k)` равна 100 % (в таблице — округление до 0.01 п.п.)."
        print "- Левая колонка — без инструментов, правая — с киркой и лампой."
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
        print "## 4. Ограничения"
        print ""
        print "- Уровень профессии равен требуемому: шанс добычи 95 %, время 2 с. При более низком уровне шанс падает, время растёт — камни/час будут ниже."
        print "- Учитывается одна жила; переходы между жилами, конкуренция и простои не моделируются."
        print "- Средний респавн — `(min_s + max_s) / 2`; фактическое время случайно."
        print "- Двойная руда от лампы на камни не влияет; распределение типов камней (`mixed_share`) здесь не считается."
        print "- Точные значения — биномиальное распределение; камень выпадает только при успешной добыче."
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

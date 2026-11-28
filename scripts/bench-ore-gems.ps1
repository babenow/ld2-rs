# Бенчмарк рудных жил: сколько камней падает и как часто (M39).
# Реализация для Windows; запускать через scripts\bench-ore-gems.bat.
# Отчёт — docs/benchmarks/ore-gems.md (файл в .gitignore).

param(
    [switch]$Stdout
)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

$OutPath = Join-Path $Root 'docs/benchmarks/ore-gems.md'
$ConfigPath = Join-Path $Root 'crates/ld2-server/server.example.toml'
$LocationsPath = Join-Path $Root 'content/locations.json'
$ItemsPath = Join-Path $Root 'content/items.json'
$Culture = [System.Globalization.CultureInfo]::InvariantCulture

function Read-Cfg([string]$Key) {
    $inside = $false
    foreach ($line in Get-Content -LiteralPath $ConfigPath -Encoding UTF8) {
        if ($line -match '^\[gameplay\.professions\]') {
            $inside = $true
            continue
        }
        if ($line -match '^\[') {
            $inside = $false
        }
        if ($inside -and $line -match "^\s*$Key\s*=\s*([0-9.]+)") {
            return [double]::Parse($matches[1], $Culture)
        }
    }
    throw "не найдена настройка $Key в $ConfigPath"
}

$Base = Read-Cfg 'mining_chance_base'
$PerLevel = Read-Cfg 'mining_chance_per_level'
$MinChance = Read-Cfg 'mining_chance_min'
$MasterMult = Read-Cfg 'mining_master_gem_mult'
$LampAdd = Read-Cfg 'mining_lamp_gem_bonus'

function Format-Num([double]$Value, [int]$Digits = 2) {
    $format = "{0:F$Digits}"
    return [string]::Format($Culture, $format, $Value)
}

function Format-Pct([double]$Value, [int]$Digits = 2) {
    return (Format-Num ($Value * 100.0) $Digits) + '%'
}

function Get-Comb([int]$N, [int]$K) {
    $r = 1.0
    for ($i = 1; $i -le $K; $i++) {
        $r = $r * ($N - $K + $i) / $i
    }
    return $r
}

function Get-Pmf([int]$N, [double]$P, [int]$K) {
    return (Get-Comb $N $K) * [math]::Pow($P, $K) * [math]::Pow(1.0 - $P, $N - $K)
}

function Get-GemChance([double]$Chance, [double]$Mult, [double]$Add) {
    return [math]::Min([math]::Max($Chance * $Mult + $Add, 0.0), 1.0)
}

function Get-Metrics([double]$Chance, [double]$Mult, [double]$Add, [int]$Stock, [double]$Rmin, [double]$Rmax) {
    $pg = Get-GemChance $Chance $Mult $Add
    $pa = $Base * $pg
    $expected = $Stock * $pa
    $p0 = [math]::Pow(1.0 - $pa, $Stock)
    $cycle = $Stock * 2.0 + ($Rmin + $Rmax) / 2.0
    $gph = 0.0
    if ($cycle -gt 0) {
        $gph = $expected * 3600.0 / $cycle
    }
    return [pscustomobject]@{
        PGem = $pg
        PAttempt = $pa
        Expected = $expected
        P0 = $p0
        Cycle = $cycle
        GemsPerHour = $gph
    }
}

$Items = Get-Content -LiteralPath $ItemsPath -Raw -Encoding UTF8 | ConvertFrom-Json
$Locations = Get-Content -LiteralPath $LocationsPath -Raw -Encoding UTF8 | ConvertFrom-Json
$Names = @{}
foreach ($item in $Items.items) {
    if ($item.id -like 'ore_*') {
        $Names[$item.id] = $item.name
    }
}

$Order = @(
    'ore_copper', 'ore_tin', 'ore_iron', 'ore_bronze', 'ore_silver',
    'ore_gold', 'ore_platinum', 'ore_adamantite', 'ore_mithril'
)
$Veins = @{}
foreach ($loc in $Locations.locations) {
    foreach ($entity in $loc.entities) {
        if ($null -eq $entity.gems -or $null -eq $entity.resource) {
            continue
        }
        if ($Order -notcontains $entity.resource) {
            continue
        }
        $stock = if ($null -ne $entity.max) { [int]$entity.max } else { 1 }
        $required = if ($null -ne $entity.required_level) { [int]$entity.required_level } else { 1 }
        $chance = [double]$entity.gems.chance
        $chanceMult = if ($null -ne $entity.gems.chance_mult) { [double]$entity.gems.chance_mult } else { 1.0 }
        $chance = $chance * $chanceMult
        $rmin = if ($null -ne $entity.respawn -and $null -ne $entity.respawn.min_s) { [double]$entity.respawn.min_s } else { 60.0 }
        $rmax = if ($null -ne $entity.respawn -and $null -ne $entity.respawn.max_s) { [double]$entity.respawn.max_s } else { 120.0 }
        $current = $Veins[$entity.resource]
        if ($null -eq $current -or $stock -gt $current.Stock) {
            $Veins[$entity.resource] = [pscustomobject]@{
                Stock = $stock
                Required = $required
                Chance = $chance
                Rmin = $rmin
                Rmax = $rmax
            }
        }
    }
}

$Lines = New-Object System.Collections.Generic.List[string]
function Add-Line([string]$Text = '') {
    $Lines.Add($Text)
}

Add-Line '# Бенчмарк рудных жил — камни (M39)'
Add-Line ''
Add-Line 'Сгенерировано `scripts/bench-ore-gems.sh` / `bench-ore-gems.bat` по `content/locations.json` и `server.example.toml`.'
Add-Line ''
Add-Line '## Допущения'
Add-Line ''
Add-Line '- уровень профессии = требуемому уровню жилы: гейт пройден, добыча 2 с, шанс успеха 95 %;'
Add-Line '- провал добычи (5 %) тоже списывает 1 запас жилы, но камня не даёт;'
Add-Line '- одна попытка = одна единица запаса; камень — 1 шт. за успешный бросок;'
Add-Line '- шанс жилы = `gems.chance × gems.chance_mult`; по умолчанию `chance_mult = 1.0`;'
Add-Line '- «Камней/час» — одна жила: полный запас × 2 с + средний респавн; без переходов между жилами;'
Add-Line '- для руды взята самая ёмкая жила (у олова в лесу есть жила с запасом 2);'
Add-Line ("- инструменты: кирка ×{0}, лампа +{1} п.п. (`mining_master_gem_mult` / `mining_lamp_gem_bonus`)." -f (Format-Num $MasterMult 0), (Format-Num ($LampAdd * 100) 0))
Add-Line ''
Add-Line '## 1. Без инструментов'
Add-Line ''
Add-Line '### Как читать таблицу 1'
Add-Line ''
Add-Line '- **Шанс/успех** — шанс камня в одном успешном сборе: `gems.chance × gems.chance_mult` у жилы.'
Add-Line '- **Шанс/попытка** — с учётом 5 % провала добычи: `0.95 × шанс/успех`. Провал тоже списывает 1 запас, поэтому это вероятность на единицу запаса.'
Add-Line '- **1 камень в N попыток** — `1 / шанс/попытка`: сколько в среднем единиц запаса нужно пройти до первого камня.'
Add-Line '- **Камней/жила** — ожидание для полной жилы: `запас × шанс/попытка`.'
Add-Line '- **≥1 камень/жила** — `1 − P(0)`: шанс, что за полный запас выпадет хотя бы один камень.'
Add-Line '- **Камней/час** — одна жила без переходов: `камней/жила × 3600 / (запас × 2 с + средний респавн)`.'
Add-Line ''
Add-Line 'Пример для меди: 3 единицы запаса × 0.59375% = 0.018 камня с жилы; один камень в среднем раз в 168.4 попытки; P(0) = 98.2%; цикл 3 × 2 с + 90 с = 96 с → 0.67 камня/час.'
Add-Line ''
Add-Line '| Руда | Ур. | Запас жилы | Шанс/успех | Шанс/попытка | 1 камень в N попыток | Камней/жила | ≥1 камень/жила | Камней/час |'
Add-Line '|---|---:|---:|---:|---:|---:|---:|---:|---:|'

foreach ($ore in $Order) {
    if (-not $Veins.ContainsKey($ore)) { continue }
    $v = $Veins[$ore]
    $m = Get-Metrics $v.Chance 1.0 0.0 $v.Stock $v.Rmin $v.Rmax
    $oneIn = if ($m.PAttempt -gt 0) { Format-Num (1.0 / $m.PAttempt) 1 } else { '∞' }
    Add-Line ("| {0} | {1} | {2} | {3} | {4} | {5} | {6} | {7} | {8} |" -f `
        $Names[$ore], $v.Required, $v.Stock,
        (Format-Pct $m.PGem 2), (Format-Pct $m.PAttempt 2), $oneIn,
        (Format-Num $m.Expected 3), (Format-Pct (1.0 - $m.P0) 1), (Format-Num $m.GemsPerHour 2))
}
Add-Line ''
Add-Line '## 2. Эффект инструментов'
Add-Line ''
Add-Line '### Как читать таблицу 2'
Add-Line ''
Add-Line 'В каждой ячейке — **шанс камня за успешную добычу / ожидаемое число камней с полной жилы**.'
Add-Line ''
Add-Line '- **Без инструментов** — базовый шанс жилы.'
Add-Line '- **Кирка мастера** — `min(1, base × mining_master_gem_mult)`; по умолчанию ×2.'
Add-Line '- **Лампа шахтёра** — `min(1, base + mining_lamp_gem_bonus)`; по умолчанию +2 п.п.'
Add-Line '- **Кирка + лампа** — `min(1, base × mult + add)`. Ожидание уже включает 95 % успеха и полный запас жилы.'
Add-Line ''
Add-Line 'Сравнивать колонки стоит по обоим числам: шанс показывает «частоту события», а камней/жила — сколько в среднем получит игрок за один цикл.'
Add-Line ''
Add-Line '| Руда | Без инструментов | Кирка мастера | Лампа шахтёра | Кирка + лампа |'
Add-Line '|---|---|---|---|---|'
$ToolDefs = @(
    @{ Mult = 1.0; Add = 0.0 },
    @{ Mult = $MasterMult; Add = 0.0 },
    @{ Mult = 1.0; Add = $LampAdd },
    @{ Mult = $MasterMult; Add = $LampAdd }
)
foreach ($ore in $Order) {
    if (-not $Veins.ContainsKey($ore)) { continue }
    $v = $Veins[$ore]
    $cells = @()
    foreach ($tool in $ToolDefs) {
        $m = Get-Metrics $v.Chance $tool.Mult $tool.Add $v.Stock $v.Rmin $v.Rmax
        $cells += ((Format-Pct $m.PGem 2) + ' / ' + (Format-Num $m.Expected 3))
    }
    Add-Line ("| {0} | {1} |" -f $Names[$ore], ($cells -join ' | '))
}
Add-Line ''
Add-Line '## 3. Распределение камней с жилы'
Add-Line ''
Add-Line '### Как читать таблицу 3'
Add-Line ''
Add-Line 'Столбцы `P(0..N)` — вероятности получить ровно `k` камней за полный запас жилы, где `N` — запас. Считается по биномиальному распределению:'
Add-Line ''
Add-Line '`P(k) = C(N,k) × p^k × (1−p)^(N−k)`, где `p` — шанс/попытка из таблицы 1.'
Add-Line ''
Add-Line '- `P(0)` — жила не дала ни одного камня.'
Add-Line '- `P(1)` — ровно один камень, `P(2)` — ровно два и т. д.'
Add-Line '- Сумма всех `P(k)` равна 100 % (в таблице — округление до 0.01 п.п.).'
Add-Line '- Левая колонка — без инструментов, правая — с киркой и лампой.'
Add-Line ''
Add-Line '| Руда | N | Без инструментов P(0..N) | Кирка + лампа P(0..N) |'
Add-Line '|---|---:|---|---|'
foreach ($ore in $Order) {
    if (-not $Veins.ContainsKey($ore)) { continue }
    $v = $Veins[$ore]
    $plain = Get-Metrics $v.Chance 1.0 0.0 $v.Stock $v.Rmin $v.Rmax
    $both = Get-Metrics $v.Chance $MasterMult $LampAdd $v.Stock $v.Rmin $v.Rmax
    $plainParts = @()
    $bothParts = @()
    for ($k = 0; $k -le $v.Stock; $k++) {
        $plainParts += (Format-Pct (Get-Pmf $v.Stock $plain.PAttempt $k) 2)
        $bothParts += (Format-Pct (Get-Pmf $v.Stock $both.PAttempt $k) 2)
    }
    Add-Line ("| {0} | {1} | {2} | {3} |" -f `
        $Names[$ore], $v.Stock, ($plainParts -join ' / '), ($bothParts -join ' / '))
}
Add-Line ''
Add-Line '## 4. Ограничения'
Add-Line ''
Add-Line '- Уровень профессии равен требуемому: шанс добычи 95 %, время 2 с. При более низком уровне шанс падает, время растёт — камни/час будут ниже.'
Add-Line '- Учитывается одна жила; переходы между жилами, конкуренция и простои не моделируются.'
Add-Line '- Средний респавн — `(min_s + max_s) / 2`; фактическое время случайно.'
Add-Line '- Двойная руда от лампы на камни не влияет; распределение типов камней (`mixed_share`) здесь не считается.'
Add-Line '- Точные значения — биномиальное распределение; камень выпадает только при успешной добыче.'

$Text = ($Lines -join [Environment]::NewLine) + [Environment]::NewLine
if ($Stdout) {
    Write-Output $Text
} else {
    $dir = Split-Path -Parent $OutPath
    if (-not (Test-Path -LiteralPath $dir)) {
        New-Item -ItemType Directory -Path $dir | Out-Null
    }
    [System.IO.File]::WriteAllText($OutPath, $Text, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "записано: docs/benchmarks/ore-gems.md"
}

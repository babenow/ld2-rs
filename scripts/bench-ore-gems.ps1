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
Add-Line '- «Камней/час» — одна жила: полный запас × 2 с + средний респавн; без переходов между жилами;'
Add-Line '- для руды взята самая ёмкая жила (у олова в лесу есть жила с запасом 2);'
Add-Line ("- инструменты: кирка ×{0}, лампа +{1} п.п. (`mining_master_gem_mult` / `mining_lamp_gem_bonus`)." -f (Format-Num $MasterMult 0), (Format-Num ($LampAdd * 100) 0))
Add-Line ''
Add-Line '## 1. Без инструментов'
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
Add-Line 'Шанс камня за успешную добычу / ожидаемое число камней с полной жилы.'
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
Add-Line 'Вероятности P(k) для обычной добычи и с киркой+лампой; N — запас жилы.'
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
Add-Line 'Точные значения — биномиальное распределение; камень выпадает только при успешной добыче.'

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

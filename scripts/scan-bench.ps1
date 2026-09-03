# scan-bench.ps1
#
# One full scan, measured and recorded. This is the automated half of the
# performance test group in docs/manual-test-plan.md (group X): it builds the
# headless binary, runs one real scan over the machine's real libraries, reads
# the numbers out of gametrimmer.log, appends an entry to the test log, and
# compares that entry against the last run made under the same conditions.
#
#   pwsh -File scripts/scan-bench.ps1
#   pwsh -File scripts/scan-bench.ps1 -SkipBuild -Note "після відкату 6c40470"
#
# Exit codes:
#   0  the run is within the previous run's range (or is the first entry)
#   1  a regression: the run got slower on the same workload
#   2  the run could not be made or measured (nothing was recorded)
#
# It deliberately does NOT touch the database: an empty database and a
# populated one are different workloads, not a knob to turn, and clearing the
# owner's real database would also drop the manually added libraries that make
# the workload what it is. The state is read out of the log and recorded, and
# entries are only ever compared against entries with the same state.

#Requires -Version 7

[CmdletBinding()]
param(
    # Reuse the bench exe already in dist/ instead of rebuilding it.
    [switch]$SkipBuild,
    # Free-text note stored with the entry ("cold cache", "after X", ...).
    [string]$Note = '',
    # Parse an existing log instead of running anything, print the entry it
    # would record and write nothing. This is how the parser is checked
    # against a real log after the scan's log lines change - the one thing
    # here that silently rots otherwise.
    [string]$LogOnly = ''
)

$ErrorActionPreference = 'Stop'

# Every number this script writes is read back by its own comparison pass, so
# formatting must not follow the machine's locale: on a uk-UA machine "85.2s"
# is written "85,2s", the parser's [0-9.]+ never matches it, and every run
# would silently report as the first one with nothing to compare against.
[Threading.Thread]::CurrentThread.CurrentCulture = [Globalization.CultureInfo]::InvariantCulture

# A total this much slower than the comparable previous run counts as a
# regression rather than machine noise. The writer stage alone has measured
# 17.6 s, 20.5 s and 26.3 s on identical work, so a tighter bound would cry
# wolf on every second run.
$TotalRegressionPct = 10
# Per-stage growth worth naming in the verdict even when the total held.
$StageRegressionPct = 25

$RepoRoot = Split-Path -Parent $PSScriptRoot
$DistDir = Join-Path $RepoRoot 'dist'
$BenchExe = Join-Path $DistDir 'gametrimmer-bench.exe'
$LogFile = Join-Path $DistDir 'gametrimmer.log'
$IniFile = Join-Path $DistDir 'gametrimmer.ini'
$TestLog = Join-Path $RepoRoot 'docs/internal/scan-test-log.md'

function Fail([string]$Message) {
    Write-Host "ЗУПИНКА: $Message" -ForegroundColor Red
    exit 2
}

function Say([string]$Message) { Write-Host $Message -ForegroundColor Cyan }

# Rust's Duration Debug formatting ("85.1810517s", "525.3241ms", "52.3µs")
# into plain seconds. Returns $null for anything unrecognized so a changed log
# line shows up as a missing figure rather than as a silent zero.
function ConvertTo-Seconds([string]$Text) {
    if ([string]::IsNullOrWhiteSpace($Text)) { return $null }
    if ($Text -notmatch '^\s*([0-9]+(?:\.[0-9]+)?)\s*(ns|µs|us|ms|s)\s*$') { return $null }
    $value = [double]$Matches[1]
    switch ($Matches[2]) {
        'ns' { return $value / 1e9 }
        'µs' { return $value / 1e6 }
        'us' { return $value / 1e6 }
        'ms' { return $value / 1e3 }
        's' { return $value }
    }
    return $null
}

function Format-Seconds([object]$Seconds) {
    if ($null -eq $Seconds) { return '?' }
    return ('{0:0.0}s' -f [double]$Seconds)
}

# ---------------------------------------------------------------- preflight

if ($LogOnly) {
    if (-not (Test-Path $LogOnly)) { Fail "нема $LogOnly" }
    $LogFile = $LogOnly
    $commit = 'n/a'
    $subject = '(розбір наявного журналу)'
    $dirty = 'n/a'
    $wallClock = [TimeSpan]::Zero
    $runStart = [datetime]::MinValue
}

# The MFT path needs raw volume read access. A run without it falls back to
# walking directories, which is a different scan and a different class of
# numbers - recording it beside elevated runs would poison every later
# comparison.
if (-not $LogOnly) {
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Fail 'потрібні права адміністратора - без них скан не читає $MFT і числа непорівнянні.'
}

# Two processes over one gametrimmer.db is exactly what the single-instance
# guard refuses, and a scan running beside this one would skew both.
$running = Get-Process -Name 'gametrimmer', 'gametrimmer-bench' -ErrorAction SilentlyContinue
if ($running) {
    Fail 'GameTrimmer уже запущено - закрий застосунок і повтори (одна база на два процеси).'
}

if (-not (Test-Path $IniFile)) {
    Fail "нема $IniFile - запусти застосунок один раз, щоб він створив налаштування й знайшов бібліотеки."
}
if ((Get-Content $IniFile -Raw) -notmatch '(?m)^logging_enabled\s*=\s*true\s*$') {
    Fail 'у налаштуваннях вимкнено журнал (logging_enabled) - без нього прогін нічим міряти.'
}

$commit = (git -C $RepoRoot rev-parse --short HEAD).Trim()
$subject = (git -C $RepoRoot log -1 --pretty=%s).Trim()
$dirty = if ((git -C $RepoRoot status --porcelain)) { 'dirty' } else { 'clean' }

# ------------------------------------------------------------------- build

if (-not $SkipBuild) {
    Say 'Збираю headless-збірку...'
    Push-Location $RepoRoot
    try {
        cargo build --release -p gametrimmer --features headless
        if ($LASTEXITCODE -ne 0) { Fail 'збірка впала.' }
    }
    finally { Pop-Location }

    New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
    Copy-Item (Join-Path $RepoRoot 'target/release/gametrimmer.exe') $BenchExe -Force
}
if (-not (Test-Path $BenchExe)) {
    Fail "нема $BenchExe - прожени без -SkipBuild."
}
}

# --------------------------------------------------------------------- run

if (-not $LogOnly) {
$reportPath = Join-Path ([IO.Path]::GetTempPath()) "gametrimmer-bench-$([Guid]::NewGuid().ToString('N')).txt"
$runStart = Get-Date
Say "Ганяю скан ($BenchExe --scan)..."
$process = Start-Process -FilePath $BenchExe -ArgumentList '--scan', '--report', $reportPath -Wait -PassThru
$wallClock = (Get-Date) - $runStart
Remove-Item $reportPath -ErrorAction SilentlyContinue

if ($process.ExitCode -ne 0) {
    Fail "скан завершився з кодом $($process.ExitCode) - нічого не записано."
}
}

# ------------------------------------------------------------------- parse

if (-not (Test-Path $LogFile)) { Fail "нема $LogFile - журнал не писався." }
$lines = Get-Content $LogFile

# Everything from the last "Scan started" on: the logger keeps one previous
# session, so anchoring on the marker is safer than an offset taken before the
# run (which a rotation would invalidate).
$startIndex = -1
for ($i = $lines.Count - 1; $i -ge 0; $i--) {
    if ($lines[$i] -match 'Scan started') { $startIndex = $i; break }
}
if ($startIndex -lt 0) { Fail 'у журналі нема рядка "Scan started" - схоже, журнал вимкнено.' }

$runLines = $lines[$startIndex..($lines.Count - 1)]

# Guard against parsing a stale session: the entry must be the run just made.
if (-not $LogOnly -and $runLines[0] -match '^\[([^\]]+)\]') {
    $logged = [datetimeoffset]::Parse($Matches[1])
    if ($logged.LocalDateTime -lt $runStart.AddMinutes(-1)) {
        Fail 'останній запис у журналі старіший за цей прогін - журнал не оновився.'
    }
}

function Find-Line([string]$Pattern) {
    for ($i = $runLines.Count - 1; $i -ge 0; $i--) {
        if ($runLines[$i] -match $Pattern) { return $runLines[$i] }
    }
    return ''
}

$doneLine = Find-Line 'Scan done in'
if (-not $doneLine) { Fail 'у журналі нема рядка "Scan done in" - скан не дійшов до кінця.' }

$total = $scan = $analyze = $housekeeping = $findings = $null
if ($doneLine -match 'Scan done in (\S+?) \(scan (\S+?) and analyze (\S+?) overlap') {
    $total = ConvertTo-Seconds $Matches[1]
    $scan = ConvertTo-Seconds $Matches[2]
    $analyze = ConvertTo-Seconds $Matches[3]
}
if ($doneLine -match 'housekeeping (\S+?)\)') { $housekeeping = ConvertTo-Seconds $Matches[1] }
if ($doneLine -match 'findings: (\d+)') { $findings = [int]$Matches[1] }
if ($null -eq $total) { Fail 'рядок "Scan done in" не розібрався - формат журналу змінився, онови парсер.' }

$dbState = 'unknown'
$envLine = Find-Line 'Environment:'
if ($envLine -match 'database (.+)$') { $dbState = $Matches[1].Trim() }
# "holds generation 7 (410 file rows to supersede)" varies run to run; the
# comparable fact is empty vs. populated, so entries are keyed on that.
$dbKey = if ($dbState -eq 'empty') { 'empty' } elseif ($dbState -like 'holds generation*') { 'populated' } else { 'unknown' }

$libraries = '?'
$vendors = '?'
$games = '?'
$libLine = Find-Line 'Libraries: '
if ($libLine -match 'Libraries: (\d+) \(([^)]*)\), games: (\d+)') {
    $libraries = $Matches[1]; $vendors = $Matches[2]; $games = [int]$Matches[3]
}
elseif ($libLine -match 'Libraries: (\d+), games: (\d+)') {
    # A binary from before the vendor tally was logged.
    $libraries = $Matches[1]; $vendors = 'типи не в журналі'; $games = [int]$Matches[2]
}

$mft = '?'
$walkdir = '?'
if ((Find-Line 'MFT pass:') -match 'MFT pass: (\d+) via MFT, (\d+) via walkdir') {
    $mft = $Matches[1]; $walkdir = $Matches[2]
}

$orphans = '?'
if ((Find-Line 'Orphans:') -match 'Orphans: (\d+) found') { $orphans = $Matches[1] }
$artifacts = '?'
if ((Find-Line 'Janitor:') -match 'Janitor: (\d+) artifacts found') { $artifacts = $Matches[1] }
$activate = $null
if ((Find-Line 'Generation activated in') -match 'activated in (\S+)$') { $activate = ConvertTo-Seconds $Matches[1] }
$prune = $null
if ((Find-Line 'Superseded generation pruned in') -match 'pruned in (\S+?) \(') { $prune = ConvertTo-Seconds $Matches[1] }
$wal = '?'
if ((Find-Line 'before the final checkpoint') -match 'WAL (.+?) before the final checkpoint, (.+?) after') {
    $wal = "$($Matches[1]) -> $($Matches[2])"
}

$workers = '?'
$stages = [ordered]@{}
$stageLine = Find-Line 'Stage CPU time'
if ($stageLine) {
    if ($stageLine -match 'across (\d+) workers') { $workers = $Matches[1] }
    foreach ($match in [regex]::Matches($stageLine, '([a-z_]+) ([0-9.]+(?:ns|µs|us|ms|s)) \(')) {
        $stages[$match.Groups[1].Value] = ConvertTo-Seconds $match.Groups[2].Value
    }
}

$writer = '?'
$writerLine = Find-Line 'Writer breakdown'
if ($writerLine -match 'sql (\S+) \(\d+%\), commit (\S+) \(\d+%\),\s+row building (\S+) \(') {
    $writer = 'sql={0} commit={1} rows={2}' -f (Format-Seconds (ConvertTo-Seconds $Matches[1])),
    (Format-Seconds (ConvertTo-Seconds $Matches[2])), (Format-Seconds (ConvertTo-Seconds $Matches[3]))
}

# --------------------------------------------------------------- comparison

# The previous entry made under the same conditions. A different database
# state is a different workload, and comparing across them invents regressions
# that are not there.
$previous = $null
if (Test-Path $TestLog) {
    $entries = ((Get-Content $TestLog -Raw) -split '(?m)^## ') | Where-Object { $_ -match 'db=' }
    foreach ($entry in $entries) {
        if ($entry -notmatch 'db=(\S+)' -or $Matches[1] -ne $dbKey) { continue }
        $candidate = @{}
        if ($entry -match '(?m)^- Ігри: (\d+) \| Знахідки: (\d+)') {
            $candidate.games = [int]$Matches[1]; $candidate.findings = [int]$Matches[2]
        }
        if ($entry -match '(?m)^- Час: total=([0-9.]+)s') { $candidate.total = [double]$Matches[1] }
        if ($entry -match '(?m)^- Стадії[^:]*: (.+)$') {
            $stageMap = @{}
            foreach ($match in [regex]::Matches($Matches[1], '([a-z_]+)=([0-9.]+)s')) {
                $stageMap[$match.Groups[1].Value] = [double]$match.Groups[2].Value
            }
            $candidate.stages = $stageMap
        }
        if ($entry -match '^(\S+ \S+)') { $candidate.stamp = $Matches[1] }
        if ($candidate.total) { $previous = $candidate }
    }
}

$verdict = 'перший запис для цих умов - порівнювати нема з чим'
$regression = $false
if ($previous) {
    $sameWorkload = ($previous.games -eq $games) -and ($previous.findings -eq $findings)
    $deltaPct = if ($previous.total -gt 0) { ($total - $previous.total) / $previous.total * 100 } else { 0 }
    $movement = '{0} -> {1} ({2:+0.0;-0.0;0}%)' -f (Format-Seconds $previous.total), (Format-Seconds $total), $deltaPct

    $slowStages = @()
    if ($previous.stages) {
        foreach ($name in $stages.Keys) {
            $was = $previous.stages[$name]
            if (-not $was -or $was -le 0) { continue }
            $growth = ($stages[$name] - $was) / $was * 100
            if ($growth -ge $StageRegressionPct) {
                $slowStages += ('{0} {1} -> {2} (+{3:0}%)' -f $name, (Format-Seconds $was), (Format-Seconds $stages[$name]), $growth)
            }
        }
    }

    if (-not $sameWorkload) {
        $verdict = "інше навантаження (було ігор $($previous.games), знахідок $($previous.findings)), total $movement - не регресія"
    }
    elseif ($deltaPct -ge $TotalRegressionPct) {
        $regression = $true
        $verdict = "РЕГРЕСІЯ: total $movement на тому самому навантаженні"
        if ($slowStages) { $verdict += '; стадії: ' + ($slowStages -join ', ') }
    }
    elseif ($slowStages) {
        $verdict = "total $movement у межах норми, але подорожчали стадії: " + ($slowStages -join ', ')
    }
    else {
        $verdict = "у межах попереднього прогону, total $movement"
    }
}

# ------------------------------------------------------------------- record

$stageText = (($stages.Keys | ForEach-Object { '{0}={1}' -f $_, (Format-Seconds $stages[$_]) }) -join ' ')
$noteLine = if ($Note) { "`n- Нотатка: $Note" } else { '' }

$entryText = @"

## $((Get-Date).ToString('yyyy-MM-dd HH:mm')) — ``$commit`` — $subject

- Прогін: headless --scan, дерево $dirty, з правами адміністратора, процес $([int]$wallClock.TotalSeconds) s
- Умови: db=$dbKey ($dbState), workers=$workers
- Бібліотеки: $libraries ($vendors)
- Ігри: $games | Знахідки: $findings | MFT/walkdir: $mft/$walkdir | Сироти: $orphans | Артефакти: $artifacts
- Час: total=$(Format-Seconds $total) scan=$(Format-Seconds $scan) analyze=$(Format-Seconds $analyze) housekeeping=$(Format-Seconds $housekeeping) activate=$(Format-Seconds $activate) prune=$(Format-Seconds $prune)
- Стадії (CPU, $workers воркерів): $stageText
- Писар: $writer
- WAL: $wal$noteLine
- Висновок: $verdict
"@

if (-not $LogOnly) { Add-Content -Path $TestLog -Value $entryText -Encoding utf8 }

Write-Host ''
Write-Host $entryText.Trim()
Write-Host ''
$where = if ($LogOnly) { 'Розбір журналу без запису (-LogOnly).' } else { "Записано у $TestLog" }
if ($regression) {
    Write-Host $verdict -ForegroundColor Red
    Write-Host $where -ForegroundColor Red
    exit 1
}
Say $where
exit 0

# A live tour of qirc in eight screens, about three and a half minutes.
# The programs come from the playground, web/index.html, so this and the
# Showcase button at angadbasandrai.github.io/qirc always run the same files.
# Every run is computed before the first screen, so each one appears at once.
#
#   pwsh scripts/showcase.ps1            or   powershell -File scripts\showcase.ps1
#
# Right arrow, space or Enter moves on, left arrow goes back, 1 to 8 jump,
# R redraws the screen, T restarts the clock, Q or Escape quits.
# -Trust adds a ninth screen that runs the test suite, and -Print draws every
# screen one after another without waiting, to rehearse or check the output.
param(
    [switch] $NoBuild,
    [switch] $Trust,
    [switch] $Print
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $root
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$windows = [Environment]::OSVersion.Platform -eq "Win32NT"
if (-not $NoBuild) {
    cargo build --release --quiet
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}
$qirc = Join-Path $root ("target/release/qirc" + $(if ($windows) { ".exe" } else { "" }))
$scratch = Join-Path $root "target/showcase"
New-Item -ItemType Directory -Force $scratch | Out-Null
$invariant = [Globalization.CultureInfo]::InvariantCulture

$block = [string] [char] 0x2588
$eighths = @("", [char] 0x258F, [char] 0x258E, [char] 0x258D, [char] 0x258C, [char] 0x258B, [char] 0x258A, [char] 0x2589)
$arrow = [string] [char] 0x2192
$check = [string] [char] 0x2713
$cross = [string] [char] 0x2717
$minus = [string] [char] 0x2212

# ------------------------------------------------------------------ syntax colours

$llvm = @{
    Syntax  = [regex] '(?<c>;.*$|//.*$)|(?<s>c?"[^"]*")|(?<l>^[\w.$-]+:)|(?<q>@__quantum__qis__[\w.]+)|(?<r>@__quantum__rt__[\w.]+)|(?<g>@[\w.$-]+)|(?<t>%(?:Qubit|Result)\**|\b(?:void|i1|i8|i32|i64|double|ptr)\b\**)|(?<k>\b(?:define|call|br|ret|phi|icmp|add|sub|load|getelementptr|inttoptr|label|internal|constant|eq|ne|slt|sgt|nuw|nsw|align|to|null|OPENQASM|include|qubit|bit|qreg|creg|input|float|int|const|for|in|measure)\b)|(?<v>%[\w.$-]+)|(?<n>-?\b\d+(?:\.\d+)?\b)'
    Kinds   = "c", "s", "l", "q", "r", "g", "t", "k", "v", "n"
    Palette = @{ c = "DarkGray"; s = "DarkYellow"; l = "Yellow"; q = "Green"; r = "DarkGreen"; g = "Cyan"; t = "DarkCyan"; k = "Magenta"; v = "White"; n = "Cyan" }
}
$ir = @{
    Syntax  = [regex] '(?<h>^program\b.*)|(?<l>^[\w.]+:)|(?<m>\bmeasure\b|->)|(?<g>^\s+(?!measure\b)[a-z]+\b)|(?<q>\bq\d+\b)|(?<r>\br\d+\b)|(?<n>-?\b\d+(?:\.\d+)?\b)'
    Kinds   = "h", "l", "m", "g", "q", "r", "n"
    Palette = @{ h = "DarkGray"; l = "Yellow"; m = "Magenta"; g = "Green"; q = "Cyan"; r = "DarkYellow"; n = "White" }
}
$report = @{
    Syntax  = [regex] '(?<e>^error(\[[A-Z0-9]+\])?:.*)|(?<w>^warning:.*)|(?<a>\^+)|(?<p>-->|^\s*= note:.*)|(?<s>^(source|kernel|program|noise):)|(?<b>\|[01]+>)|(?<n>-?\b\d+\.\d+(e[-+]?\d+)?\b)'
    Kinds   = "e", "w", "a", "p", "s", "b", "n"
    Palette = @{ e = "Red"; w = "Yellow"; a = "Red"; p = "Cyan"; s = "DarkGray"; b = "Cyan"; n = "Green" }
}
$plain = @{ Syntax = [regex] '(?!)'; Kinds = @(); Palette = @{} }

# ------------------------------------------------------------------ programs and runs

function Save-Programs {
    $page = [IO.File]::ReadAllText((Join-Path $root "web/index.html"))
    $pattern = '<script type="text/plain"[^>]*data-(?:file|calibration)="([^"]+)"[^>]*>\r?\n(.*?)</script>'
    foreach ($match in [regex]::Matches($page, $pattern, "Singleline")) {
        [IO.File]::WriteAllText((Join-Path $scratch $match.Groups[1].Value), $match.Groups[2].Value)
    }
    # The two calibrations the playground has built in.
    $line5 = [IO.File]::ReadAllText((Join-Path $root "examples/line5.cal"))
    [IO.File]::WriteAllText((Join-Path $scratch "line5.cal"), $line5)
    $detuning = (0..4 | ForEach-Object { "detuning $_ 300" }) -join "`n"
    [IO.File]::WriteAllText((Join-Path $scratch "detuned.cal"), $line5 + $detuning + "`n")
}

$runs = @{}

function Invoke-Run {
    param([string] $Command)

    $words = @($Command -split " " | Where-Object { $_ })
    if ($words[0] -ne "explain" -and $words[0] -ne "surface") {
        $words += "--color", "never"
    }
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $qirc
    $info.Arguments = $words -join " "
    $info.WorkingDirectory = $scratch
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = [Text.Encoding]::UTF8
    $info.StandardErrorEncoding = [Text.Encoding]::UTF8
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $process = [Diagnostics.Process]::Start($info)
    $errors = $process.StandardError.ReadToEndAsync()
    $output = $process.StandardOutput.ReadToEnd()
    $process.WaitForExit()
    $clock.Stop()
    [pscustomobject] @{
        Out  = $output -replace "`r", ""
        Err  = $errors.Result -replace "`r", ""
        Code = $process.ExitCode
        Ms   = $clock.Elapsed.TotalMilliseconds
    }
}

function Get-Run {
    param([string] $Command)

    if (-not $runs.ContainsKey($Command)) {
        $runs[$Command] = Invoke-Run $Command
    }
    $runs[$Command]
}

function Split-Lines {
    param([string] $Text)

    @(($Text -replace "\n+$", "") -split "\n")
}

function Get-Counts {
    param([string] $Text, [string] $Section = "measurement")

    $counts = New-Object System.Collections.Generic.List[object]
    $inside = $false
    foreach ($line in (Split-Lines $Text)) {
        if ($line -match '^(\w+)(?: measurement)? over \d+ shots') {
            $inside = $Matches[1] -eq $Section
        } elseif ($inside -and $line -match '^\s+(.+?)\s+(\d+)\s+([\d.]+)\s*$') {
            $counts.Add([pscustomobject] @{ Key = $Matches[1].Trim(); Count = [int] $Matches[2]; Share = [double]::Parse($Matches[3], $invariant) })
        } elseif ($inside -and $line.Trim() -and $line -notmatch 'standard error') {
            $inside = $false
        }
    }
    $counts.ToArray()
}

function Get-Field {
    param([string] $Text, [string] $Name)

    foreach ($line in (Split-Lines $Text)) {
        $trimmed = $line.TrimStart()
        if ($trimmed.StartsWith($Name)) {
            return $trimmed.Substring($Name.Length).Trim()
        }
    }
    ""
}

function Get-Shape {
    param([string] $Text)

    if ($Text -match '(\d+) qubits, (\d+) results, (\d+) gates, depth (\d+)') {
        return [pscustomobject] @{ Qubits = [int] $Matches[1]; Gates = [int] $Matches[3]; Depth = [int] $Matches[4] }
    }
    [pscustomobject] @{ Qubits = 0; Gates = 0; Depth = 0 }
}

function Get-Cost {
    param([string] $Text)

    $row = Split-Lines $Text | Where-Object { $_ -match '^(entry|worst case)\b' } | Select-Object -Last 1
    $numbers = @([regex]::Matches("$row", '\d+') | ForEach-Object { [int] $_.Value })
    [pscustomobject] @{ T = $numbers[0]; Cx = $numbers[1]; Gates = $numbers[2] }
}

function Format-Text {
    param([string] $Format, [object[]] $Values)

    [string]::Format($invariant, $Format, $Values)
}

function Format-Ms {
    param([double] $Value)

    if ($Value -lt 1000) { return Format-Text "{0:0} ms" $Value }
    Format-Text "{0:0.00} s" ($Value / 1000)
}

# ------------------------------------------------------------------ drawing

function Get-Width {
    try {
        $width = $Host.UI.RawUI.WindowSize.Width
        if ($width -gt 60) { return $width - 1 }
    } catch {}
    159
}

function Get-Height {
    try {
        $height = $Host.UI.RawUI.WindowSize.Height
        if ($height -gt 20) { return $height }
    } catch {}
    50
}

function Get-Fitted {
    param([string] $Line, [int] $Width)

    if ($Line.Length -le $Width) { return $Line }
    $Line.Substring(0, [math]::Max(0, $Width - 3)) + "..."
}

function Write-Tokens {
    param([string] $Line, [hashtable] $Language, [switch] $NoNewline)

    $position = 0
    foreach ($match in $Language.Syntax.Matches($Line)) {
        if ($match.Length -eq 0) { continue }
        if ($match.Index -gt $position) {
            Write-Host $Line.Substring($position, $match.Index - $position) -NoNewline -ForegroundColor Gray
        }
        $color = "Gray"
        foreach ($kind in $Language.Kinds) {
            if ($match.Groups[$kind].Success) {
                $color = $Language.Palette[$kind]
                break
            }
        }
        Write-Host $match.Value -NoNewline -ForegroundColor $color
        $position = $match.Index + $match.Length
    }
    Write-Host $Line.Substring($position) -NoNewline:$NoNewline -ForegroundColor Gray
}

function Write-Lines {
    param([string[]] $Lines, [hashtable] $Language, [int] $Indent = 2)

    $width = (Get-Width) - $Indent
    foreach ($line in $Lines) {
        Write-Host (" " * $Indent) -NoNewline
        Write-Tokens (Get-Fitted $line $width) $Language
    }
}

function Get-Source {
    param([string] $File, [string[]] $Marks = @())

    $lines = New-Object System.Collections.Generic.List[string]
    $hidden = 0
    foreach ($line in (Split-Lines ([IO.File]::ReadAllText((Join-Path $scratch $File))))) {
        if ($line -match '^\s*(declare\b|!|source_filename|target |; ModuleID|attributes #)|^%\w+ = type opaque') {
            $hidden++
            continue
        }
        if ($line.Trim() -eq "" -and ($lines.Count -eq 0 -or $lines[$lines.Count - 1].Trim() -eq "")) { continue }
        $marked = $false
        foreach ($mark in $Marks) {
            if ($line.Contains($mark)) { $marked = $true }
        }
        $lines.Add($(if ($marked) { ">" + $line } else { " " + $line }))
    }
    while ($lines.Count -gt 0 -and $lines[$lines.Count - 1].Trim() -eq "") { $lines.RemoveAt($lines.Count - 1) }
    if ($hidden -gt 0) { $lines.Add("; $hidden lines of declarations and metadata not shown") }
    , $lines.ToArray()
}

# Side by side columns, each a title, lines and a language, shrunk to fit the window.
function Show-Columns {
    param([object[]] $Columns, [int] $Rows = 0)

    $room = (Get-Width) - 2 - 3 * ($Columns.Count - 1)
    $widths = @($Columns | ForEach-Object { (@($_.Lines) + $_.Title | Measure-Object -Property Length -Maximum).Maximum })
    while (($widths | Measure-Object -Sum).Sum -gt $room) {
        $widest = 0
        for ($i = 1; $i -lt $widths.Count; $i++) {
            if ($widths[$i] -gt $widths[$widest]) { $widest = $i }
        }
        if ($widths[$widest] -le 16) { break }
        $widths[$widest]--
    }

    Write-Host "  " -NoNewline
    for ($c = 0; $c -lt $Columns.Count; $c++) {
        Write-Host (Get-Fitted $Columns[$c].Title $widths[$c]).PadRight($widths[$c]) -NoNewline -ForegroundColor Cyan
        if ($c -lt $Columns.Count - 1) { Write-Host "   " -NoNewline }
    }
    Write-Host ""

    $count = ($Columns | ForEach-Object { @($_.Lines).Count } | Measure-Object -Maximum).Maximum
    if ($Rows -gt 0) { $count = [math]::Min($count, $Rows) }
    for ($row = 0; $row -lt $count; $row++) {
        Write-Host "  " -NoNewline
        for ($c = 0; $c -lt $Columns.Count; $c++) {
            $lines = @($Columns[$c].Lines)
            $text = ""
            if ($row -lt $lines.Count) {
                $text = if ($row -eq $count - 1 -and $lines.Count -gt $count) { "... $($lines.Count - $count + 1) more lines" } else { $lines[$row] }
            }
            $text = Get-Fitted $text $widths[$c]
            if ($text.StartsWith(">")) {
                Write-Host ">" -NoNewline -ForegroundColor Red
                Write-Host $text.Substring(1) -NoNewline -ForegroundColor White -BackgroundColor DarkRed
            } else {
                Write-Tokens $text $Columns[$c].Language -NoNewline
            }
            if ($c -lt $Columns.Count - 1) {
                Write-Host (" " * ($widths[$c] - $text.Length)) -NoNewline
                Write-Host " | " -NoNewline -ForegroundColor DarkGray
            }
        }
        Write-Host ""
    }
}

function Get-BarText {
    param([double] $Share, [int] $Width)

    $Share = [math]::Max(0.0, [math]::Min(1.0, $Share))
    $cells = $Share * $Width
    $full = [math]::Floor($cells)
    $part = [int] [math]::Floor(($cells - $full) * 8)
    $text = ($block * $full) + $eighths[$part]
    if ($Share -gt 0 -and $text.Length -eq 0) { $text = $eighths[1] }
    $text
}

function Write-Bar {
    param([double] $Share, [string] $Color, [int] $Width)

    $text = Get-BarText $Share $Width
    Write-Host $text -NoNewline -ForegroundColor $Color
    Write-Host (" " * ($Width - $text.Length)) -NoNewline
}

function Write-BarRow {
    param([string] $Label, [int] $LabelWidth, [double] $Share, [string] $Color, [string] $Value, [int] $Width = 50)

    Write-Host ("  " + $Label.PadRight($LabelWidth)) -NoNewline -ForegroundColor White
    Write-Bar $Share $Color $Width
    Write-Host ("  " + $Value) -ForegroundColor Gray
}

function Write-Chip {
    param([string] $Text, [string] $Color = "Cyan")

    Write-Host " $Text " -NoNewline -ForegroundColor Black -BackgroundColor $Color
    Write-Host "  " -NoNewline
}

function Write-Verdict {
    param($Run)

    if ($Run.Code -eq 0 -and $Run.Out.StartsWith("equivalent")) {
        Write-Host "$check qirc diff: equivalent" -NoNewline -ForegroundColor Green
    } else {
        Write-Host ("$cross " + @(Split-Lines $Run.Out)[0]) -NoNewline -ForegroundColor Red
    }
}

function Write-Note {
    param([string] $Text)

    Write-Host ("  " + $Text) -ForegroundColor DarkGray
}

# ------------------------------------------------------------------ the beats

$observable = "Z0*Z1*+*0.5*X0*+*0.5*X1"
$star = "--gates rz-sx-cx --coupling line:4"

$beats = @(
    @{
        Title   = "From a quantum program to results"
        Caption = "A Q# style loop and helper function: run at compile time, flattened to four gates, simulated, and written out as OpenQASM 3"
        Seconds = 30
        Show    = "qsharp_loop.ll --shots 1000 --seed 7", "qsharp_loop.ll --emit qasm3"
        Jobs    = @{ run = "qsharp_loop.ll --shots 1000 --seed 7"; qasm = "qsharp_loop.ll --emit qasm3" }
        Body    = {
            param($r)
            $kept = Split-Lines $r.run.Out | Where-Object { $_ -notmatch '^\s*P\(q|^simulated in|standard error' }
            Show-Columns @(
                @{ Title = "qsharp_loop.ll, the input"; Lines = (Get-Source "qsharp_loop.ll"); Language = $llvm }
                @{ Title = "qirc output"; Lines = @($kept); Language = $report }
                @{ Title = "--emit qasm3"; Lines = (Split-Lines $r.qasm.Out); Language = $llvm }
            ) -Rows ((Get-Height) - 16)
            Write-Host ""
            foreach ($count in (Get-Counts $r.run.Out)) {
                Write-BarRow $count.Key 6 $count.Share "Green" (Format-Text "{0,6:0.0}%" (100 * $count.Share))
            }
            Write-Host ""
            $shape = Get-Shape $r.run.Out
            Write-Host "  " -NoNewline
            Write-Chip "loop unrolled at compile time"
            Write-Chip "$($shape.Gates) gates, depth $($shape.Depth)" "DarkGray"
            Write-Chip (Format-Ms $r.run.Ms) "DarkGray"
            Write-Host ""
        }
    }
    @{
        Title   = "The optimiser, and proof it changed nothing"
        Caption = "Gates and T gates before and after optimisation, each result checked against the original by qirc diff"
        Seconds = 30
        Show    = "redundant.ll -O3 -v --emit check", "mod5_4.qasm --gates h,s,t,cx -O2 --emit cost", "diff mod5_4.qasm -O2 --gates h,s,t,cx"
        Jobs    = @{
            r0 = "redundant.ll -O0 --emit check"; r3 = "redundant.ll -O3 -v --emit check"; rd = "diff redundant.ll -O3"
            i0 = "redundant.ll -O0 --emit ir"; i3 = "redundant.ll -O3 --emit ir"
            p0 = "pyqir_simple.ll --gates rz-sx-cx -O0 --emit check"; p3 = "pyqir_simple.ll --gates rz-sx-cx -O3 --emit check"; pd = "diff pyqir_simple.ll -O3 --gates rz-sx-cx"
            m0 = "mod5_4.qasm --gates h,s,t,cx -O0 --emit cost"; m2 = "mod5_4.qasm --gates h,s,t,cx -O2 --emit cost"; md = "diff mod5_4.qasm -O2 --gates h,s,t,cx"
            l2 = "ladder.qasm --gates h,s,t,cx -O2 --emit cost"; ld = "diff ladder.qasm -O2 --gates h,s,t,cx"
        }
        Body    = {
            param($r)
            $toffolis = @(Get-Content (Join-Path $scratch "ladder.qasm") | Where-Object { $_ -match '^\s*ccx\b' }).Count
            $rows = @(
                @{ Label = "redundant.ll, gates"; A = (Get-Shape $r.r0.Out).Gates; B = (Get-Shape $r.r3.Out).Gates; Proof = $r.rd }
                @{ Label = "PyQIR sample, gates in rz sx cx"; A = (Get-Shape $r.p0.Out).Gates; B = (Get-Shape $r.p3.Out).Gates; Proof = $r.pd }
                @{ Label = "mod5_4, T gates"; A = (Get-Cost $r.m0.Out).T; B = (Get-Cost $r.m2.Out).T; Proof = $r.md }
                @{ Label = "Toffoli ladder, T gates"; A = 7 * $toffolis; B = (Get-Cost $r.l2.Out).T; Proof = $r.ld }
            )
            Write-Host ("  " + "".PadRight(34) + "before".PadLeft(7) + "after".PadLeft(7)) -ForegroundColor Cyan
            foreach ($row in $rows) {
                Write-Host ("  " + $row.Label.PadRight(34)) -NoNewline -ForegroundColor White
                Write-Host ("$($row.A)".PadLeft(7)) -NoNewline -ForegroundColor Gray
                Write-Host ("$($row.B)".PadLeft(7) + "  ") -NoNewline -ForegroundColor Green
                Write-Bar ($row.B / $row.A) "Green" 24
                Write-Host (Format-Text "{0,6}   " ($minus + [math]::Round(100 * ($row.A - $row.B) / $row.A) + "%")) -NoNewline -ForegroundColor Green
                Write-Verdict $row.Proof
                Write-Host ""
            }
            Write-Note "The bar is what is left. T gates are the expensive gate on error corrected hardware."
            Write-Host ""
            $passes = Split-Lines $r.r3.Err | Where-Object { $_ -match '^\s+[\w-]+: \d+$' } | ForEach-Object { $_.Trim() }
            Show-Columns @(
                @{ Title = "redundant.ll at -O0"; Lines = (Split-Lines $r.i0.Out); Language = $ir }
                @{ Title = "at -O3"; Lines = (Split-Lines $r.i3.Out); Language = $ir }
                @{ Title = "passes that fired"; Lines = @($passes); Language = $report }
            )
        }
    }
    @{
        Title   = "Mistakes are reported at the source"
        Caption = "A program that claims the Base Profile but branches on a measurement: a code, the exact line, and a longer explanation"
        Seconds = 15
        Show    = "bad_profile.ll", "explain QIR0300", "qsharp_loop.ll --emit qsam3"
        Jobs    = @{ bad = "bad_profile.ll"; explain = "explain QIR0300"; typo = "qsharp_loop.ll --emit qsam3" }
        Body    = {
            param($r)
            $output = @(Split-Lines $r.bad.Err) + "" + @(Split-Lines $r.typo.Err)[0] + "" + (Split-Lines $r.explain.Out)
            Show-Columns @(
                @{ Title = "qirc output"; Lines = $output; Language = $report }
                @{ Title = "bad_profile.ll"; Lines = (Get-Source "bad_profile.ll" @("entry:", "read_result__body(%Result* null)")); Language = $llvm }
            )
        }
    }
    @{
        Title   = "Ready for real hardware"
        Caption = "Qubit 0 talks to every other qubit, but on a line of four only neighbours interact and the chip only has rz, sx and cx"
        Seconds = 25
        Show    = "star.qasm --emit circuit $star", "star.qasm --shots 100000 $star", "diff star.qasm $star"
        Jobs    = @{
            c0 = "star.qasm --emit circuit"; c1 = "star.qasm --emit circuit $star"
            s0 = "star.qasm --emit check"; s1 = "star.qasm --emit check $star"
            h0 = "star.qasm --shots 100000 --no-state"; h1 = "star.qasm --shots 100000 --no-state $star"
            d = "diff star.qasm $star"
        }
        Body    = {
            param($r)
            $a = Get-Shape $r.s0.Out
            $b = Get-Shape $r.s1.Out
            Write-Host "  as written: $($a.Gates) gates, depth $($a.Depth)" -ForegroundColor Cyan
            Write-Lines (Split-Lines $r.c0.Out) $report
            Write-Host ""
            Write-Host "  on a line of 4 in rz, sx, cx: $($b.Gates) gates, depth $($b.Depth)" -ForegroundColor Cyan
            Write-Lines (Split-Lines $r.c1.Out) $report
            Write-Host ""
            Write-Host "  100,000 shots each:  " -NoNewline -ForegroundColor Cyan
            Write-Host "ideal  " -NoNewline -ForegroundColor DarkGray
            Write-Host "routed onto the chip" -ForegroundColor Green
            $chip = @{}
            foreach ($count in (Get-Counts $r.h1.Out)) { $chip[$count.Key] = $count.Share }
            foreach ($count in (Get-Counts $r.h0.Out)) {
                $routed = if ($chip.ContainsKey($count.Key)) { $chip[$count.Key] } else { 0 }
                Write-BarRow $count.Key 6 $count.Share "DarkGray" (Format-Text "{0,5:0.0}%" (100 * $count.Share)) 60
                Write-BarRow "" 6 $routed "Green" (Format-Text "{0,5:0.0}%" (100 * $routed)) 60
            }
            Write-Host ""
            Write-Host "  " -NoNewline
            Write-Verdict $r.d
            Write-Host ""
        }
    }
    @{
        Title   = "Past 30 qubits"
        Caption = "A state vector doubles with every qubit, so qirc looks at the program first and picks a simulator that fits it. 1000 shots each"
        Seconds = 25
        Show    = "ghz1000.ll --shots 1000", "adder.qasm --shots 1000", "chain60.qasm --bond 32 --shots 1000"
        Jobs    = @{
            wide = "wide.qasm --shots 1000 --no-state"; ghz = "ghz1000.ll --shots 1000 --no-state"; adder = "adder.qasm --shots 1000 --no-state"
            rank = "clifford_t40.qasm --shots 1000 --no-state"; cut = "halves.qasm --shots 1000 --no-state"; mps = "chain60.qasm --bond 32 --shots 1000 --no-state"
        }
        Body    = {
            param($r)
            $rows = @(
                @("wide", "rotation spread by CNOTs"), @("ghz", "GHZ state"), @("adder", "32 bit adder"),
                @("rank", "Clifford chain with 3 T gates"), @("cut", "two 20 qubit halves, 2 CNOTs apart"), @("mps", "rotations along a chain, --bond 32")
            )
            $longest = ($rows | ForEach-Object { $r[$_[0]].Ms } | Measure-Object -Maximum).Maximum
            Write-Host ("  " + "".PadRight(38) + "qubits".PadLeft(7) + "   " + "kernel".PadRight(28) + "time, log scale") -ForegroundColor Cyan
            foreach ($row in $rows) {
                $run = $r[$row[0]]
                $kernel = Get-Field $run.Out "kernel:"
                $qubits = (Get-Shape $run.Out).Qubits
                Write-Host ("  " + $row[1].PadRight(38)) -NoNewline -ForegroundColor White
                Write-Host ((Format-Text "{0:N0}" $qubits).PadLeft(7) + "   ") -NoNewline -ForegroundColor Gray
                Write-Host $kernel.PadRight(28) -NoNewline -ForegroundColor Green
                Write-Bar ([math]::Log10(1 + $run.Ms) / [math]::Log10(1 + $longest)) "Cyan" 30
                Write-Host ("  " + (Format-Ms $run.Ms)) -ForegroundColor Gray
            }
            Write-Host ""
            $ghz = (Get-Counts $r.ghz.Out | ForEach-Object { Format-Text "{0} {1:0}%" $_.Key, (100 * $_.Share) }) -join " / "
            Write-Host "  1000 qubit GHZ state:  " -NoNewline -ForegroundColor White
            Write-Host $ghz -ForegroundColor Green
            Write-Note "A state vector would need 2^1000 amplitudes. The tableau stores 2n x 2n bits, about half a megabyte."
            $bits = @(Get-Counts $r.adder.Out)[0].Key.ToCharArray()
            [array]::Reverse($bits)
            [long] $sum = 0
            foreach ($bit in $bits) { $sum = $sum * 2 + [int] [string] $bit }
            Write-Host ""
            Write-Host "  66 qubit adder:        " -NoNewline -ForegroundColor White
            Write-Host $sum.ToString("N0", $invariant) -NoNewline -ForegroundColor Green
            Write-Host "   = 3,141,592,653 + 2,718,281,828, read from the sum register" -ForegroundColor DarkGray
        }
    }
    @{
        Title   = "Real devices are noisy"
        Caption = "A five qubit GHZ state should read 00000 or 11111. With a device's errors it often does not, and qirc has the standard fixes"
        Seconds = 30
        Show    = "ghz5_measured.qasm --shots 4000 --calibration readout.cal --noisy --mitigate", "ghz5.qasm --calibration line5.cal --noisy --observable Z0*Z4 --zne", "idle.qasm --calibration detuned.cal --noisy --dd"
        Jobs    = @{
            ideal = "ghz5_measured.qasm --shots 4000 --no-state"; noisy = "ghz5_measured.qasm --shots 4000 --no-state --calibration line5.cal --noisy"
            readout = "ghz5_measured.qasm --shots 4000 --no-state --calibration readout.cal --noisy"; mitigated = "ghz5_measured.qasm --shots 4000 --no-state --calibration readout.cal --noisy --mitigate"
            zne = "ghz5.qasm --calibration line5.cal --noisy --observable Z0*Z4 --zne --shots 4000 --no-state --seed 2"
            drift = "idle.qasm --shots 4000 --no-state --calibration detuned.cal --noisy"; echo = "idle.qasm --shots 4000 --no-state --calibration detuned.cal --noisy --dd"
        }
        Body    = {
            param($r)
            $good = {
                param($text)
                $total = 0.0
                foreach ($count in (Get-Counts $text)) {
                    if ($count.Key -eq "00000" -or $count.Key -eq "11111") { $total += $count.Share }
                }
                $total
            }
            $mitigated = 0.0
            $inside = $false
            foreach ($line in (Split-Lines $r.mitigated.Out)) {
                if ($line -match '^mitigated measurement') { $inside = $true }
                elseif ($inside -and $line -match '^\s+(00000|11111)\s+([\d.]+)') { $mitigated += [double]::Parse($Matches[2], $invariant) }
                elseif ($inside -and $line -notmatch '^\s+[01]+\s') { $inside = $false }
            }
            Write-Host "  share of shots reading 00000 or 11111" -ForegroundColor Cyan
            foreach ($row in @(
                    @("ideal", (& $good $r.ideal.Out), "DarkGray"),
                    @("line5.cal device", (& $good $r.noisy.Out), "Yellow"),
                    @("readout errors of 5 to 15%", (& $good $r.readout.Out), "Red"),
                    @("the same, --mitigate", $mitigated, "Green"))) {
                Write-BarRow $row[0] 28 $row[1] $row[2] (Format-Text "{0,6:0.0}%" (100 * $row[1]))
            }
            Write-Host ""
            $noisy = @([regex]::Matches((Get-Field $r.zne.Out "noisy expectation at 1x, 2x and 3x noise over 4000 shots:"), '([\d.]+) \u00b1 ([\d.]+)'))
            $zero = [regex]::Match((Get-Field $r.zne.Out "zero noise extrapolation:"), '([\d.]+) \u00b1 ([\d.]+)')
            Write-Host "  zero noise extrapolation of Z0 Z4, exact value 1" -ForegroundColor Cyan
            for ($i = $noisy.Count - 1; $i -ge 0; $i--) {
                $value = [double]::Parse($noisy[$i].Groups[1].Value, $invariant)
                Write-BarRow "$($i + 1)x noise" 28 (($value - 0.7) / 0.3) "Yellow" (Format-Text "{0:0.000} {1} {2}" $value, ([char] 0xB1), $noisy[$i].Groups[2].Value)
            }
            $value = [double]::Parse($zero.Groups[1].Value, $invariant)
            Write-BarRow "extrapolated to zero" 28 (($value - 0.7) / 0.3) "Green" (Format-Text "{0:0.000} {1} {2}" $value, ([char] 0xB1), $zero.Groups[2].Value)
            Write-Note "bars start at 0.7"
            Write-Host ""
            Write-Host "  echo pulses: qubit 0 drifts at 300 kHz while it waits; the right answer is 0" -ForegroundColor Cyan
            foreach ($row in @(@("detuned, waiting", $r.drift, "Yellow"), @("with echo pulses, --dd", $r.echo, "Green"))) {
                $share = (Get-Counts $row[1].Out | Where-Object { $_.Key -eq "0" } | Select-Object -First 1).Share
                Write-BarRow $row[0] 28 $share $row[2] (Format-Text "{0,6:0.0}%" (100 * $share))
            }
        }
    }
    @{
        Title   = "Planning for error corrected machines"
        Caption = "T gates, code distance, factories, physical qubits and runtime from the compiled program, then the surface code itself, simulated and decoded"
        Seconds = 25
        Show    = "mod5_4.qasm --emit resources -O2", "surface --error 0.001 --shots 200000"
        Jobs    = @{ o0 = "mod5_4.qasm --emit resources -O0"; o2 = "mod5_4.qasm --emit resources -O2"; surface = "surface --error 0.001 --shots 200000" }
        Body    = {
            param($r)
            Write-Host ("  mod5_4 at a physical error of 0.1%".PadRight(36) + "-O0".PadLeft(14) + "-O2".PadLeft(14)) -ForegroundColor Cyan
            foreach ($name in "T gates", "magic states", "code distance", "physical qubits", "runtime") {
                $old = (Get-Field $r.o0.Out $name) -replace ' \(.*$', '' -replace ', \d[\d,]* cycles.*$', '' -replace ', as .*$', ''
                $new = (Get-Field $r.o2.Out $name) -replace ' \(.*$', '' -replace ', \d[\d,]* cycles.*$', '' -replace ', as .*$', ''
                Write-Host ("  " + $name.PadRight(34)) -NoNewline -ForegroundColor White
                Write-Host ($old.PadLeft(14)) -NoNewline -ForegroundColor Gray
                Write-Host ($new.PadLeft(14)) -ForegroundColor Green
            }
            Write-Note "Fewer T gates means fewer magic state factories: the optimiser shrinks the machine."
            Write-Host ""
            Write-Host "  surface code memory at 0.1%, logical error per round, log scale" -ForegroundColor Cyan
            foreach ($line in (Split-Lines $r.surface.Out)) {
                if ($line -match '^\s+(\d+)\s+\d+\s+\d+\s+\d+\s+([\d.e-]+)\s+([\d.e-]+)') {
                    $simulated = [double]::Parse($Matches[2], $invariant)
                    $model = [double]::Parse($Matches[3], $invariant)
                    $scale = { param($v) (6 + [math]::Log10($v)) / 3 }
                    Write-BarRow "d = $($Matches[1]), simulated" 22 (& $scale $simulated) "Green" $Matches[2] 48
                    Write-BarRow "        model" 22 (& $scale $model) "DarkGray" $Matches[3] 48
                }
            }
            Write-Note "200,000 shots per distance, 64 at a time in each machine word, decoded by union find."
        }
    }
    @{
        Title   = "Variational circuits"
        Caption = "OpenQASM 3 inputs become parameters: the ground energy of Z0 Z1 + 0.5 X0 + 0.5 X1, exactly and on a noisy device"
        Seconds = 20
        Show    = "ansatz.qasm --observable `"Z0 Z1 + 0.5 X0 + 0.5 X1`" --minimize", "ansatz.qasm --observable `"...`" --minimize --noisy --calibration pair.cal"
        Jobs    = @{ exact = "ansatz.qasm --observable $observable --minimize"; noisy = "ansatz.qasm --observable $observable --minimize --noisy --calibration pair.cal --seed 5" }
        Body    = {
            param($r)
            $steps = {
                param($text)
                foreach ($line in (Split-Lines $text)) {
                    if ($line -match '^\s+(\d+)\s+(-?[\d.]+)') { , @([int] $Matches[1], [double]::Parse($Matches[2], $invariant)) }
                }
            }
            $sample = {
                param($points)
                $picked = @()
                $last = -1
                for ($k = 0; $k -lt 9; $k++) {
                    $at = [int] [math]::Round($k * ($points.Count - 1) / 8)
                    if ($at -ne $last) { $picked += , $points[$at] }
                    $last = $at
                }
                $picked
            }
            $exact = @(& $sample @(& $steps $r.exact.Out))
            $noisy = @(& $sample @(& $steps $r.noisy.Out))
            Write-Host ("  " + "exact, Adam on exact gradients".PadRight(60) + "noisy device, SPSA on 1000 shot samples") -ForegroundColor Cyan
            $rows = [math]::Max($exact.Count, $noisy.Count)
            for ($i = 0; $i -lt $rows; $i++) {
                Write-Host "  " -NoNewline
                foreach ($pair in @(@($exact, "Green"), @($noisy, "Yellow"))) {
                    $points = $pair[0]
                    if ($i -lt $points.Count) {
                        Write-Host ("step " + "$($points[$i][0])".PadLeft(3) + "  ") -NoNewline -ForegroundColor DarkGray
                        Write-Bar ((1.2 - $points[$i][1]) / 2.8) $pair[1] 36
                        Write-Host (Format-Text "{0,10:0.000000}" $points[$i][1]) -NoNewline -ForegroundColor Gray
                        Write-Host (" " * 4) -NoNewline
                    } else {
                        Write-Host (" " * 60) -NoNewline
                    }
                }
                Write-Host ""
            }
            Write-Note "bars grow as the energy falls, from 1.2 down to -1.6"
            Write-Host ""
            $minimum = [regex]::Match($r.exact.Out, 'minimum (-?[\d.]+) after (\d+) steps')
            Write-Host "  exact minimum   " -NoNewline -ForegroundColor White
            Write-Host $minimum.Groups[1].Value -NoNewline -ForegroundColor Green
            Write-Host "   after $($minimum.Groups[2].Value) steps; -sqrt(2) = -1.414214" -ForegroundColor DarkGray
            Write-Host "  noisy device    " -NoNewline -ForegroundColor White
            Write-Host (Get-Field $r.noisy.Out "noisy value") -NoNewline -ForegroundColor Yellow
            Write-Host "   the best the noisy device can show, at nearly the exact angles" -ForegroundColor DarkGray
        }
    }
)

if ($Trust) {
    $beats += @{
        Title   = "Why you can trust it"
        Caption = "The full test suite, run now"
        Seconds = 30
        Show    = @("cargo test --release")
        Jobs    = @{}
        Body    = {
            param($r)
            $passed = 0
            $failed = 0
            foreach ($line in (& cargo test --release 2>&1 | ForEach-Object { "$_" })) {
                if ($line -match '^test result: \w+\. (\d+) passed; (\d+) failed') {
                    $passed += [int] $Matches[1]
                    $failed += [int] $Matches[2]
                }
            }
            $color = if ($failed -eq 0) { "Green" } else { "Red" }
            Write-Host "  $passed tests passed, $failed failed" -ForegroundColor $color
        }
    }
}

# ------------------------------------------------------------------ presenter

$total = ($beats | ForEach-Object { $_.Seconds } | Measure-Object -Sum).Sum
$clockStart = $null

function Format-Clock {
    param([double] $Seconds)

    $s = [int] [math]::Floor([math]::Max(0.0, $Seconds))
    "{0}:{1:00}" -f [math]::Floor($s / 60), ($s % 60)
}

function Write-Header {
    param([int] $Index)

    if (-not $Print) { Clear-Host } else { Write-Host "" }
    $beat = $beats[$Index]
    Write-Host " qirc " -NoNewline -ForegroundColor Black -BackgroundColor Cyan
    Write-Host ("  " + ($Index + 1) + "/" + $beats.Count + "  ") -NoNewline -ForegroundColor DarkGray
    $title = $beat.Title
    $elapsed = ([DateTime]::Now - $script:clockStart).TotalSeconds
    $budget = ($beats[0..$Index] | ForEach-Object { $_.Seconds } | Measure-Object -Sum).Sum
    $clock = (Format-Clock $elapsed) + " of " + (Format-Clock $total)
    Write-Host $title -NoNewline -ForegroundColor White
    $gap = [math]::Max(2, (Get-Width) - 14 - $title.Length - $clock.Length)
    Write-Host (" " * $gap) -NoNewline
    Write-Host $clock -ForegroundColor $(if ($elapsed -gt $budget + 5) { "Yellow" } else { "DarkGray" })

    $width = (Get-Width) - 2
    Write-Host "  " -NoNewline
    for ($i = 0; $i -lt $beats.Count; $i++) {
        $cells = [math]::Max(1, [int] [math]::Floor($width * $beats[$i].Seconds / $total) - 1)
        $color = if ($i -lt $Index) { "DarkCyan" } elseif ($i -eq $Index) { "Cyan" } else { "DarkGray" }
        Write-Host ([string] [char] 0x2501 * $cells) -NoNewline -ForegroundColor $color
        Write-Host " " -NoNewline
    }
    Write-Host ""
    Write-Host ("  " + $beat.Caption) -ForegroundColor DarkGray
    Write-Host ""
    foreach ($command in $beat.Show) {
        Write-Host "  > " -NoNewline -ForegroundColor DarkGray
        foreach ($char in ("qirc " + $command -replace '^qirc cargo', 'cargo').ToCharArray()) {
            Write-Host $char -NoNewline -ForegroundColor White
            Start-Sleep -Milliseconds 6
        }
        Write-Host ""
    }
    Write-Host ""
}

function Wait-Key {
    $key = [Console]::ReadKey($true)
    if ($key.Key -eq "LeftArrow" -or $key.Key -eq "Backspace" -or $key.Key -eq "PageUp") { return "back" }
    if ($key.Key -eq "Escape" -or $key.Key -eq "Q") { return "quit" }
    if ($key.Key -eq "R") { return "redraw" }
    if ($key.Key -eq "T") { return "clock" }
    if ($key.KeyChar -ge "1" -and $key.KeyChar -le "9") { return "jump:" + $key.KeyChar }
    "next"
}

function Show-Beat {
    param([int] $Index)

    Write-Header $Index
    $r = @{}
    foreach ($name in $beats[$Index].Jobs.Keys) { $r[$name] = Get-Run $beats[$Index].Jobs[$name] }
    try {
        & $beats[$Index].Body $r
    } catch {
        Write-Host ("  this screen could not be drawn: " + $_.Exception.Message) -ForegroundColor Red
    }
    if ($r.Count -gt 0) {
        $compute = ($r.Values | ForEach-Object { $_.Ms } | Measure-Object -Sum).Sum
        Write-Host ""
        Write-Host ("  " + $r.Count + " runs, " + (Format-Ms $compute) + " of compute") -ForegroundColor DarkGray
    }
}

Save-Programs
$jobs = @($beats | ForEach-Object { $_.Jobs.Values })
Clear-Host
Write-Host " qirc " -NoNewline -ForegroundColor Black -BackgroundColor Cyan
Write-Host "  live showcase, $($beats.Count) screens in about $(Format-Clock $total)" -ForegroundColor White
Write-Host ""
for ($i = 0; $i -lt $jobs.Count; $i++) {
    Write-Host ("`r  computing ahead: " + ($i + 1) + " of " + $jobs.Count + " runs   ") -NoNewline -ForegroundColor DarkGray
    Get-Run $jobs[$i] | Out-Null
}
$compute = ($runs.Values | ForEach-Object { $_.Ms } | Measure-Object -Sum).Sum
Write-Host ("`r  " + $jobs.Count + " runs computed in " + (Format-Ms $compute) + "            ") -ForegroundColor DarkGray
Write-Host ""
for ($i = 0; $i -lt $beats.Count; $i++) {
    Write-Host ("  " + ($i + 1) + "  ") -NoNewline -ForegroundColor Cyan
    Write-Host $beats[$i].Title.PadRight(48) -NoNewline -ForegroundColor White
    Write-Host ("" + $beats[$i].Seconds + " s") -ForegroundColor DarkGray
}
Write-Host ""
if ($Print) {
    $clockStart = [DateTime]::Now
    for ($i = 0; $i -lt $beats.Count; $i++) { Show-Beat $i }
    return
}
Write-Host "  press any key to start" -ForegroundColor DarkGray
[void] [Console]::ReadKey($true)

$clockStart = [DateTime]::Now
$current = 0
while ($current -lt $beats.Count) {
    Show-Beat $current
    $action = Wait-Key
    if ($action -eq "quit") { break }
    elseif ($action -eq "back") { $current = [math]::Max(0, $current - 1) }
    elseif ($action -eq "redraw") { }
    elseif ($action -eq "clock") { $clockStart = [DateTime]::Now }
    elseif ($action.StartsWith("jump:")) { $current = [math]::Min($beats.Count, [int] $action.Substring(5)) - 1 }
    else { $current++ }
}

Clear-Host
Write-Host ""
Write-Host " qirc " -NoNewline -ForegroundColor Black -BackgroundColor Cyan
Write-Host "  angadbasandrai.github.io/qirc   github.com/AngadBasandrai/qirc" -ForegroundColor White
Write-Host ""
Write-Host "  cargo install qirc-compiler      pip install qirc" -ForegroundColor DarkGray
Write-Host ""

<#
.SYNOPSIS
Measures how much stack a Windows program uses, and which functions use it.

.DESCRIPTION
Runs a program under cdb and reports on its stack, in one of three modes:

  Overflow  Waits for the first stack overflow and prints the stack at that moment, each frame
            with its size, then the largest frames. Pair it with -Stack to force an overflow at a
            given reserve: the real binary is never touched, a patched copy runs instead.
  Peak      Prints each thread's peak stack use when the process exits. Windows commits stack
            pages on demand and never gives them back, so the committed size at exit is the
            high-water mark.
  Frames    Lists every stack frame of at least -MinFrame bytes that the run allocates, largest
            first. MSVC-target code probes any frame larger than a page through __chkstk, with
            the frame size in rax, so a conditional breakpoint there sees each large frame.

Frame sizes come from the PDB next to the program, so build with debug info (the dev profile has
it). Only the top-level process is followed, not the processes it spawns.

Needs cdb (Debugging Tools for Windows, or WinDbg from the Store) and, for -Stack, editbin from
the MSVC build tools.

.EXAMPLE
# Why does a debug mirrord.exe overflow a 1 MB stack? (offline: the empty kubeconfig keeps it
# away from any cluster, and the overflow happens before the cluster is contacted)
$env:KUBECONFIG = "$env:TEMP\empty-kubeconfig.yaml"
Set-Content $env:KUBECONFIG "apiVersion: v1`nkind: Config`nclusters: []`ncontexts: []`nusers: []"
.\scripts\win_stack_report.ps1 -Mode Overflow -Stack 1MB `
    -Exe target\x86_64-pc-windows-msvc\debug\mirrord.exe `
    -Arguments exec, --target, targetless, --, cmd.exe, /c, 'exit 0'

.EXAMPLE
.\scripts\win_stack_report.ps1 -Mode Peak -Exe target\x86_64-pc-windows-msvc\debug\mirrord.exe -Arguments verify-config, mirrord.json
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('Overflow', 'Peak', 'Frames')]
    [string]$Mode,

    # The program to run. Its PDB is looked up next to it, and in `deps` beside it.
    [Parameter(Mandatory)]
    [string]$Exe,

    # Arguments for the program.
    [string[]]$Arguments = @(),

    # Main-thread stack reserve to run with, in bytes (1MB works). Runs a patched copy.
    [long]$Stack = 0,

    # Smallest frame, in bytes, that Frames mode reports.
    [long]$MinFrame = 16KB,

    # How many of the largest frames to list.
    [int]$Top = 15,

    # Function names are cut to this many characters; generic instantiations get very long.
    [int]$NameWidth = 150,

    # Where the debugger log and any patched copy go. Defaults to a fresh temp directory.
    [string]$OutDir = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Find-Cdb {
    $candidates = @(
        (Get-Command cdb.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -First 1),
        (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\Debuggers\x64\cdb.exe'),
        # The WinDbg Store app installs cdb behind this execution alias.
        (Join-Path $env:LOCALAPPDATA 'Microsoft\WindowsApps\cdbX64.exe')
    )
    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path $candidate)) { return $candidate }
    }
    throw 'cdb not found. Install WinDbg (winget install Microsoft.WinDbg) or the Debugging Tools for Windows.'
}

function Find-Editbin {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        # Not `-latest`: the newest install may lack the C++ tools while an older one has them.
        $found = & $vswhere -products * -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\editbin.exe' |
            Sort-Object -Descending | Select-Object -First 1
        if ($found) { return $found }
    }
    $onPath = Get-Command editbin.exe -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }
    throw 'editbin not found. -Stack needs the MSVC build tools (the C++ workload of Visual Studio).'
}

# Quotes one argument the way CommandLineToArgvW reads it back.
function ConvertTo-CommandLineArgument([string]$Argument) {
    if ($Argument -and $Argument -notmatch '[\s"]') { return $Argument }
    $escaped = $Argument -replace '(\\*)"', '$1$1\"'
    $escaped = $escaped -replace '(\\+)$', '$1$1'
    return '"' + $escaped + '"'
}

# Parses `kf` output into physical frames, each with the stack it uses.
#
# `kf` prints, on each line, the distance from the previous line's stack pointer. That distance is
# the frame of the function on the previous physical line (the callee), not of the line it is
# printed on. Inline frames share their caller's frame, so they carry no size of their own.
function Get-FramesFromKf([string[]]$Lines) {
    $frames = New-Object System.Collections.Generic.List[object]
    $last = $null
    $rowPattern = '^\s*(?<mem>[0-9a-f]+)?\s+(?:(?<sp>[0-9a-f]{8}`[0-9a-f]{8})|\(Inline Function\))\s+\S+\s+(?<site>.+?)\s*$'
    foreach ($line in $Lines) {
        $match = [regex]::Match($line, $rowPattern)
        if (-not $match.Success) { continue }
        $size = if ($match.Groups['mem'].Success) { [Convert]::ToInt64($match.Groups['mem'].Value, 16) } else { 0 }
        if ($last) { $last.Size += $size }
        if ($match.Groups['sp'].Success) {
            $last = [pscustomobject]@{ Size = [long]0; Function = $match.Groups['site'].Value; Inlined = @() }
            $frames.Add($last)
        } elseif ($last) {
            $last.Inlined += $match.Groups['site'].Value
        }
    }
    return $frames
}

function Format-Size([long]$Bytes) {
    return ('{0,9:N1} KiB' -f ($Bytes / 1KB))
}

# Drops the `+0x..` offset so frames from the same function group together.
function Get-FunctionName([string]$Site) {
    return ($Site -replace '\+0x[0-9a-f]+$', '')
}

function Format-Name([string]$Name) {
    if ($Name.Length -le $NameWidth) { return $Name }
    return $Name.Substring(0, $NameWidth - 3) + '...'
}

$Exe = (Resolve-Path $Exe).Path
$exeDir = Split-Path $Exe -Parent
$exeName = Split-Path $Exe -Leaf
# The debugger names a module after its file, with anything outside [A-Za-z0-9_] turned into `_`.
$module = [IO.Path]::GetFileNameWithoutExtension($exeName) -replace '[^A-Za-z0-9_]', '_'

if (-not $OutDir) {
    $OutDir = Join-Path ([IO.Path]::GetTempPath()) ("win-stack-report-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path $OutDir).Path

$target = $Exe
if ($Stack -gt 0) {
    # The copy keeps the original's PDB signature, so the symbol path below still matches it.
    $target = Join-Path $OutDir $exeName
    Copy-Item $Exe $target -Force
    & (Find-Editbin) /nologo "/STACK:$Stack" $target
    if ($LASTEXITCODE -ne 0) { throw "editbin failed with exit code $LASTEXITCODE" }
    Write-Host "Running a copy with a $(Format-Size $Stack) main-thread stack: $target"
}

$symbolPath = "$exeDir;$(Join-Path $exeDir 'deps')"
$log = Join-Path $OutDir 'cdb.log'
$script = Join-Path $OutDir 'commands.txt'

$commands = switch ($Mode) {
    'Overflow' {
        @(
            'sxe sov',
            'g',
            '.echo ===STACK===',
            '.kframes 0x4000',
            'kf',
            # The innermost frame is the one being allocated: __chkstk holds its size in rax.
            '.printf "REQUESTED %d\n", @rax',
            '.echo ===END==='
        )
    }
    'Peak' {
        @(
            'sxe sov',
            'bp ntdll!RtlExitUserProcess',
            'g',
            '.echo ===PEAK===',
            # NT_TIB: StackBase at +0x8, StackLimit at +0x10.
            '~*e .printf "PEAK %x %d\n", @$tid, poi(@$teb+8)-poi(@$teb+0x10)',
            '.echo ===END==='
        )
    }
    'Frames' {
        @(
            ('bp {0}!__chkstk ".if (@rax >= 0x{1:x}) {{ .printf \"BIGFRAME %d \", @rax; ln poi(@rsp); gc }} .else {{ gc }}"' -f $module, $MinFrame),
            'g'
        )
    }
}
Set-Content -Path $script -Value ($commands + 'q') -Encoding Ascii

$cdbArguments = @('-y', $symbolPath, '-cf', $script, $target) + $Arguments |
    ForEach-Object { ConvertTo-CommandLineArgument $_ }
Write-Host "Running under cdb ($Mode); the log goes to $log"
$process = Start-Process -FilePath (Find-Cdb) -ArgumentList ($cdbArguments -join ' ') -NoNewWindow -Wait -PassThru `
    -RedirectStandardOutput $log -RedirectStandardError (Join-Path $OutDir 'cdb.stderr.log')
$text = Get-Content $log -Raw
$lines = Get-Content $log

switch ($Mode) {
    'Overflow' {
        if ($text -notmatch 'Stack overflow - code c00000fd') {
            Write-Host 'No stack overflow happened.'
            break
        }
        $start = [array]::IndexOf($lines, ($lines | Where-Object { $_ -match '===STACK===' } | Select-Object -First 1))
        $frames = Get-FramesFromKf ($lines | Select-Object -Skip ($start + 1))
        $total = ($frames | Measure-Object -Property Size -Sum).Sum

        Write-Host "`nStack at the overflow, innermost first ($(Format-Size $total) across $($frames.Count) frames):`n"
        $small = @()
        foreach ($frame in $frames) {
            if ($frame.Size -lt 1KB) { $small += $frame; continue }
            Write-Host ('{0}  {1}' -f (Format-Size $frame.Size), (Format-Name $frame.Function))
        }
        if ($small) {
            Write-Host ('{0}  ({1} frames under 1 KiB each; the log has them all)' -f (Format-Size ($small | Measure-Object -Property Size -Sum).Sum), $small.Count)
        }
        if ($text -match 'REQUESTED (\d+)') {
            $requested = [long]$Matches[1]
            Write-Host ("`nThe overflow hit while {0} was allocating a {1} frame." -f (Format-Name (Get-FunctionName $frames[1].Function)), (Format-Size $requested).Trim())
        }

        Write-Host "`nLargest frames:`n"
        $frames | Sort-Object Size -Descending | Select-Object -First $Top | ForEach-Object {
            Write-Host ('{0}  {1,5:P0}  {2}' -f (Format-Size $_.Size), ($_.Size / $total), (Format-Name (Get-FunctionName $_.Function)))
        }
    }
    'Peak' {
        $peaks = [regex]::Matches($text, 'PEAK ([0-9a-f]+) (\d+)') | ForEach-Object {
            [pscustomobject]@{ Thread = $_.Groups[1].Value; Peak = [long]$_.Groups[2].Value }
        }
        if (-not $peaks) {
            Write-Host 'The process never reached RtlExitUserProcess; see the log.'
            break
        }
        if ($text -match 'Stack overflow - code c00000fd') {
            Write-Host 'The run hit a stack overflow; these are the peaks at that moment.'
        }
        Write-Host "`nPeak stack per thread:`n"
        $peaks | Sort-Object Peak -Descending | ForEach-Object {
            Write-Host ('{0}  thread {1}' -f (Format-Size $_.Peak), $_.Thread)
        }
    }
    'Frames' {
        # `ln` prints `(address)   module!function+0xoffset   |  (address)   next`; sometimes on the
        # same line as the size, sometimes on the next.
        $found = [regex]::Matches($text, 'BIGFRAME (\d+)\s*\([0-9a-f`]+\)\s+(\S+)') | ForEach-Object {
            [pscustomobject]@{ Size = [long]$_.Groups[1].Value; Function = Get-FunctionName $_.Groups[2].Value }
        }
        if (-not $found) {
            Write-Host "No frame of $(Format-Size $MinFrame) or more was allocated."
            break
        }
        $byFunction = $found | Group-Object Function | ForEach-Object {
            [pscustomobject]@{
                Size     = ($_.Group | Measure-Object -Property Size -Maximum).Maximum
                Calls    = $_.Count
                Function = $_.Name
            }
        }
        Write-Host "`nFrames of $(Format-Size $MinFrame) or more, largest first:`n"
        $byFunction | Sort-Object Size -Descending | Select-Object -First $Top | ForEach-Object {
            Write-Host ('{0}  {1,5}x  {2}' -f (Format-Size $_.Size), $_.Calls, (Format-Name $_.Function))
        }
    }
}

Write-Host "`ncdb exited with $($process.ExitCode); full log: $log"

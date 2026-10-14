<#
.SYNOPSIS
CI guard for the mirrord CLI's stack use on Windows.

.DESCRIPTION
Windows gives a program's main thread only the stack its PE header asks for, and the CLI polls its
whole runtime on that thread. This script checks two things, on patched copies of a built
mirrord.exe (the original is never touched):

  1. The overflow report works: with a stack far too small, the CLI must overflow and print its
     report, every frame named and sized (`utils_win::diagnostics::stack`). The report is echoed,
     so a CI log shows what a real one looks like.
  2. The CLI fits its budget: with the stack cut to -Budget, offline runs must finish without an
     overflow. When one does overflow, the report above the failure names the frames that grew.

The copies sit next to the original, so the report finds the same PDB.

.EXAMPLE
.\scripts\win_stack_check.ps1 -Exe target\debug\mirrord.exe
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Exe,

    # The stack the CLI must fit in. Kept well under what build.rs links, so growth shows up here
    # before it reaches a user.
    [long]$Budget = 1MB,

    # Small enough that any build overflows, to exercise the report.
    [long]$TooSmall = 256KB
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Exe = (Resolve-Path $Exe).Path
$dir = Split-Path $Exe -Parent

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$editbin = & $vswhere -products * -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\editbin.exe' |
    Sort-Object -Descending | Select-Object -First 1
if (-not $editbin) { throw 'editbin not found; it comes with the MSVC build tools.' }

# An empty kubeconfig: the CLI fails at the cluster connection, after its argument parsing,
# runtime, config and kube client setup have all run.
$kubeconfig = Join-Path ([IO.Path]::GetTempPath()) 'mirrord-stack-check-kubeconfig.yaml'
Set-Content $kubeconfig "apiVersion: v1`nkind: Config`nclusters: []`ncontexts: []`nusers: []"

# Runs a copy of the CLI with the given stack. Returns its combined output and whether it overflowed.
function Invoke-WithStack([long]$Stack, [string[]]$Arguments) {
    $copy = Join-Path $dir ("mirrord-stack-{0}k.exe" -f ($Stack / 1KB))
    Copy-Item $Exe $copy -Force
    & $editbin /nologo "/STACK:$Stack" $copy | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "editbin failed with exit code $LASTEXITCODE" }

    $env:KUBECONFIG = $kubeconfig
    $env:MIRRORD_PROGRESS_MODE = 'off'
    # Windows PowerShell turns each stderr line into an error record, which `Stop` would throw on.
    $ErrorActionPreference = 'Continue'
    $output = & $copy @Arguments 2>&1 | ForEach-Object {
        if ($_ -is [System.Management.Automation.ErrorRecord]) { [string]$_.TargetObject } else { "$_" }
    }
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    Remove-Item $copy -Force -ErrorAction SilentlyContinue

    [pscustomobject]@{
        Output     = $output
        ExitCode   = $code
        Overflowed = [bool]($output -match 'has overflowed its stack')
    }
}

$runs = @(
    @{ Name = 'exec (offline)'; Arguments = @('exec', '--target', 'targetless', '--', 'cmd.exe', '/c', 'exit 0') },
    @{ Name = '--help'; Arguments = @('--help') }
)
$failed = $false

# 1. The report path.
$forced = Invoke-WithStack $TooSmall $runs[0].Arguments
Write-Host "::group::Forced overflow at $($TooSmall / 1KB) KiB: the report CI shows when the CLI overflows"
$forced.Output | ForEach-Object { Write-Host $_ }
Write-Host '::endgroup::'

$report = $forced.Output -join "`n"
$checks = [ordered]@{
    'the run overflowed'               = $forced.Overflowed
    'the report was printed'           = $report -match 'mirrord: stack overflow\. The stack, innermost frame first'
    'frames are named from the PDB'    = $report -match '\d+\.\d KiB\s+mirrord::'
    'the largest frames are ranked'    = $report -match 'Largest frames:'
}
foreach ($check in $checks.GetEnumerator()) {
    if ($check.Value) {
        Write-Host "ok: $($check.Key)"
    } else {
        Write-Host "::error title=Stack overflow report::With a $($TooSmall / 1KB) KiB stack, expected $($check.Key)."
        $failed = $true
    }
}

# 2. The budget.
foreach ($run in $runs) {
    $result = Invoke-WithStack $Budget $run.Arguments
    if ($result.Overflowed) {
        Write-Host "::group::$($run.Name) overflowed $($Budget / 1KB) KiB"
        $result.Output | ForEach-Object { Write-Host $_ }
        Write-Host '::endgroup::'
        Write-Host ("::error title=Stack budget::mirrord {0} needs more than {1} KiB of main-thread stack. " +
            "The report in the group above ranks the frames that used it; see 'Stack usage' in AGENTS.md.") -f $run.Name, ($Budget / 1KB)
        $failed = $true
    } else {
        Write-Host "ok: $($run.Name) fits in $($Budget / 1KB) KiB"
    }
}

if ($failed) { exit 1 }

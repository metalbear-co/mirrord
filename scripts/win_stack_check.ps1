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
    [long]$TooSmall = 256KB,

    # Longest a single run may take. Each one ends in seconds; a hang fails the check instead.
    [int]$TimeoutSeconds = 300
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

# Quotes one argument the way CommandLineToArgvW reads it back.
function ConvertTo-CommandLineArgument([string]$Argument) {
    if ($Argument -and $Argument -notmatch '[\s"]') { return $Argument }
    $escaped = $Argument -replace '(\\*)"', '$1$1\"'
    $escaped = $escaped -replace '(\\+)$', '$1$1'
    return '"' + $escaped + '"'
}

# Runs a copy of the CLI with the given stack, and returns its output and whether it overflowed.
#
# The output goes through files rather than PowerShell's own stderr handling, which differs between
# Windows PowerShell and PowerShell 7.
function Invoke-WithStack([long]$Stack, [string[]]$Arguments) {
    $copy = Join-Path $dir ("mirrord-stack-{0}k.exe" -f ($Stack / 1KB))
    Copy-Item $Exe $copy -Force
    & $editbin /nologo "/STACK:$Stack" $copy | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "editbin failed with exit code $LASTEXITCODE" }

    $env:KUBECONFIG = $kubeconfig
    $env:MIRRORD_PROGRESS_MODE = 'off'
    $stdout = [IO.Path]::GetTempFileName()
    $stderr = [IO.Path]::GetTempFileName()
    $process = Start-Process -FilePath $copy -NoNewWindow -PassThru `
        -ArgumentList (($Arguments | ForEach-Object { ConvertTo-CommandLineArgument $_ }) -join ' ') `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    # Without a handle taken while it runs, Windows PowerShell loses the exit code.
    $null = $process.Handle
    $finished = $process.WaitForExit($TimeoutSeconds * 1000)
    if (-not $finished) {
        $process.Kill()
        $process.WaitForExit()
    }
    $output = @(Get-Content $stdout) + @(Get-Content $stderr)
    Remove-Item $copy, $stdout, $stderr -Force -ErrorAction SilentlyContinue
    if (-not $finished) {
        $output | ForEach-Object { Write-Host $_ }
        throw "mirrord $($Arguments -join ' ') was still running after $TimeoutSeconds s; its output is above."
    }

    [pscustomobject]@{
        Output     = $output
        ExitCode   = $process.ExitCode
        Overflowed = [bool]($output -match 'has overflowed its stack')
    }
}

# Each run must reach a known end, so a run that died some other way never passes for one that fit.
$runs = @(
    @{
        Name      = 'exec (offline)'
        Arguments = @('exec', '--target', 'targetless', '--', 'cmd.exe', '/c', 'exit 0')
        # The empty kubeconfig stops it at the cluster connection.
        Expect    = 'Failed to create Kubernetes API client'
        ExitCode  = 1
    },
    @{ Name = '--help'; Arguments = @('--help'); Expect = 'Usage:'; ExitCode = 0 }
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
    'frames are named'                 = $report -match '\d+\.\d KiB\s+mirrord::'
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
        $message = ("mirrord {0} needs more than {1} KiB of main-thread stack. The report in the group " +
            "above ranks the frames that used it; see 'Stack Usage' in AGENTS.md.") -f $run.Name, ($Budget / 1KB)
        Write-Host "::error title=Stack budget::$message"
        $failed = $true
    } elseif ($result.ExitCode -ne $run.ExitCode -or -not ($result.Output -match $run.Expect)) {
        Write-Host "::group::$($run.Name) output"
        $result.Output | ForEach-Object { Write-Host $_ }
        Write-Host '::endgroup::'
        Write-Host ("::error title=Stack budget::mirrord {0} should exit {1} after printing '{2}', but exited {3}." -f
            $run.Name, $run.ExitCode, $run.Expect, $result.ExitCode)
        $failed = $true
    } else {
        Write-Host "ok: $($run.Name) fits in $($Budget / 1KB) KiB"
    }
}

if ($failed) { exit 1 }

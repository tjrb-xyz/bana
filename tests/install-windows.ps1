# install.ps1, as bana installer compiles it, on real Windows: what tests/install-ps1.sh
# cannot check on Linux. The junction bana makes without admin rights, the user PATH in the
# registry (REG_EXPAND_SZ), Unblock-File on a browser's download, the project's hook as a
# script block under a Restricted execution policy (irm | iex), and uninstall. Run it with
# the PowerShell under test (Windows PowerShell 5.1, or pwsh); each install runs in a new
# one of the same.
#
#   tests/install-windows.ps1 -Fixtures DIR    DIR: tests/install.sh --fixtures DIR's releases
param([Parameter(Mandatory = $true)][string]$Fixtures)
$ErrorActionPreference = 'Stop'
$Fixtures = (Resolve-Path -LiteralPath $Fixtures).Path
$shell = (Get-Process -Id $PID).Path
$script:n = 0; $script:fails = 0
function Check([string]$what, [scriptblock]$test) {
  $script:n++
  $ok = $false
  try { $ok = [bool](& $test) } catch { Write-Host "     | $_" }
  if ($ok) { Write-Host "ok   $what" } else { $script:fails++; Write-Host "FAIL $what"; Get-Content -LiteralPath $out -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "     | $_" } }
}

# A home of its own: LOCALAPPDATA, APPDATA and the hook's log. The user PATH is the runner's.
$T = Join-Path ([IO.Path]::GetTempPath()) ("bana-install-" + [guid]::NewGuid().ToString('N').Substring(0, 8))
$null = New-Item -ItemType Directory -Path $T
$env:LOCALAPPDATA = Join-Path $T 'local'
$env:APPDATA = Join-Path $T 'roaming'
$env:FAKE_LOG = Join-Path $T 'log'
$out = Join-Path $T 'out'
$null = New-Item -ItemType File -Path $env:FAKE_LOG
$prefix = Join-Path $env:LOCALAPPDATA 'Programs\example'
$current = Join-Path $prefix 'current'
$bin = Join-Path $current 'bin'
Write-Host "PowerShell $($PSVersionTable.PSVersion) ($shell)"

# The releases over HTTP, as GitHub (or a mirror) serves them: INSTALL_URL.
$port = 18000 + (Get-Random -Maximum 2000)
$python = (Get-Command python -ErrorAction SilentlyContinue).Source
if (-not $python) { $python = (Get-Command python3).Source }
$srv = Start-Process -FilePath $python -ArgumentList @('-m', 'http.server', "$port", '--bind', '127.0.0.1', '--directory', $Fixtures) -PassThru -WindowStyle Hidden
for ($i = 0; $i -lt 50; $i++) {
  try { $null = Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$port/" -TimeoutSec 2; break } catch { Start-Sleep -Milliseconds 200 }
}

function Invoke-Install([string]$tag, [string[]]$more) {
  $env:INSTALL_URL = "http://127.0.0.1:$port/$tag"
  & $shell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File (Join-Path $Fixtures "$tag\install.ps1") @more *> $out
  $code = $LASTEXITCODE
  Remove-Item Env:INSTALL_URL
  $code -eq 0
}
function Get-UserPathValue { (Get-Item -LiteralPath 'registry::HKEY_CURRENT_USER\Environment').GetValue('Path', '', 'DoNotExpandEnvironmentNames') }
function Get-Target([string]$link) { "$((Get-Item -LiteralPath $link -Force).Target)" }

try {
  Check "installs v1.0.0 (Invoke-WebRequest from INSTALL_URL)" { Invoke-Install 'v1.0.0' @('-Yes') }
  Check "current is a junction (no admin rights needed)" { (Get-Item -LiteralPath $current -Force).LinkType -eq 'Junction' }
  Check "... to v1.0.0" { (Get-Target $current) -eq (Join-Path $prefix 'v1.0.0') }
  Check "current\bin is on the user PATH" { (Get-UserPathValue) -split ';' -contains $bin }
  Check "... a REG_EXPAND_SZ value, as Windows keeps it" { (Get-Item -LiteralPath 'registry::HKEY_CURRENT_USER\Environment').GetValueKind('Path') -eq 'ExpandString' }
  Check "its command runs" { ((& (Join-Path $bin 'exampled.cmd')) -join '').Trim() -eq 'exampled v1.0.0 windows-x64' }
  Check "the hook: pre-install, then post-install" { ((Get-Content -LiteralPath $env:FAKE_LOG) -join '|') -like 'ps1-hook pre-install v1.0.0 *|ps1-hook post-install v1.0.0 *' }

  # A browser's download carries the web's mark (the Zone.Identifier stream).
  $zip = @(Get-ChildItem -LiteralPath (Join-Path $Fixtures 'v1.1.0') -Filter '*-windows-x64.zip')[0]
  $dl = Join-Path $T $zip.Name
  Copy-Item -LiteralPath $zip.FullName -Destination $dl
  Set-Content -LiteralPath $dl -Stream Zone.Identifier -Value "[ZoneTransfer]`r`nZoneId=3"
  Check "the download carries the web's mark" { [bool](Get-Item -LiteralPath $dl -Stream Zone.Identifier -ErrorAction SilentlyContinue) }
  Check "upgrades to v1.1.0 -From it" { Invoke-Install 'v1.1.0' @('-Yes', '-From', $dl) }
  Check "... current moved to v1.1.0" { (Get-Target $current) -eq (Join-Path $prefix 'v1.1.0') }
  Check "... and no file installed carries the mark (Unblock-File)" {
    @(Get-ChildItem -LiteralPath (Join-Path $prefix 'v1.1.0') -Recurse -File | Where-Object { Get-Item -LiteralPath $_.FullName -Stream Zone.Identifier -ErrorAction SilentlyContinue }).Count -eq 0
  }
  Check "... the PATH has current\bin once" { @((Get-UserPathValue) -split ';' | Where-Object { $_ -eq $bin }).Count -eq 1 }

  # irm | iex under a Restricted policy: no script file may run, the hook runs as a script block.
  Clear-Content -LiteralPath $env:FAKE_LOG
  $hook = Join-Path $prefix 'v1.1.0\install-hook.ps1'
  & $shell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Restricted -File $hook post-install *> $out
  Check "Restricted: a script file does not run" { $LASTEXITCODE -ne 0 -and -not (Get-Content -LiteralPath $env:FAKE_LOG) }
  $env:INSTALL_URL = "http://127.0.0.1:$port/v1.2.0"
  $env:INSTALL_YES = '1'
  $ps1 = Join-Path $Fixtures 'v1.2.0\install.ps1'
  & $shell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Restricted -Command "if ((Get-ExecutionPolicy) -ne 'Restricted') { exit 3 }; Get-Content -Raw -LiteralPath '$ps1' | Invoke-Expression" *> $out
  Remove-Item Env:INSTALL_URL, Env:INSTALL_YES
  Check "Restricted: irm | iex installs v1.2.0" { (Get-Target $current) -eq (Join-Path $prefix 'v1.2.0') }
  Check "... and the hook ran, as a script block" { ((Get-Content -LiteralPath $env:FAKE_LOG) -join '|') -like 'ps1-hook pre-install v1.2.0 prev=v1.1.0 *|ps1-hook post-install v1.2.0 prev=v1.1.0 *' }

  # Under iex, even the end (nothing to do) returns to your session: exit would close it.
  & $shell -NoLogo -NoProfile -NonInteractive -Command "Get-Content -Raw -LiteralPath '$ps1' | Invoke-Expression; Write-Host 'the session goes on'" *> $out
  Check "iex: installed already, and the session goes on" { (Get-Content -LiteralPath $out) -contains 'the session goes on' }

  Clear-Content -LiteralPath $env:FAKE_LOG
  Check "uninstall" { Invoke-Install 'v1.2.0' @('-Uninstall', '-Yes') }
  Check "... the files and the junction are gone" { -not (Test-Path -LiteralPath $prefix) }
  Check "... and current\bin is off the user PATH" { -not ((Get-UserPathValue) -split ';' -contains $bin) }
  Check "... pre-uninstall, then post-uninstall (from a copy)" { ((Get-Content -LiteralPath $env:FAKE_LOG) -join '|') -like 'ps1-hook pre-uninstall v1.2.0 *|ps1-hook post-uninstall v1.2.0 *' }
} finally {
  if ($srv) { Stop-Process -Id $srv.Id -Force -ErrorAction SilentlyContinue }
  Remove-Item -LiteralPath $T -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host "$($script:n - $script:fails) of $($script:n) passed (PowerShell $($PSVersionTable.PSVersion))"
if ($script:fails) { exit 1 }

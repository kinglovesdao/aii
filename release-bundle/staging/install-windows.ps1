# AII one-click installer for Windows (PowerShell).
#
# Run as Administrator. Builds aiid / aii / aii-mcp from source via
# rustup (no pre-compiled Windows binaries shipped in this bundle —
# no signed releases yet), installs to C:\Program Files\aii, and
# registers a Windows service via nssm or sc.exe.
#
# Usage (in an elevated PowerShell):
#   .\install-windows.ps1               # validator + join testnet
#   .\install-windows.ps1 -Observer     # RPC-only
#   .\install-windows.ps1 -Uninstall    # remove service + binaries

param(
    [switch]$Observer,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"

$InstallDir = "C:\Program Files\aii"
$DataDir    = "C:\ProgramData\aii"
$LogFile    = "$DataDir\aiid.log"
$Bootnode   = "http://8.211.135.234:8545"
$DiscoverySeeds = "8.211.135.234:30310,106.14.223.128:30310"
$DiscoveryAdvertise = if ($env:DISCOVERY_ADVERTISE) { $env:DISCOVERY_ADVERTISE } else { $env:AII_DISCOVERY_ADVERTISE }
$BftAdvertise = if ($env:BFT_ADVERTISE) { $env:BFT_ADVERTISE } else { $env:AII_BFT_ADVERTISE }
$Service    = "aiid"
$SrcRepo    = "https://github.com/kinglovesdao/aii.git"

if ($Uninstall) {
    Write-Host "[aii-install] stopping service + removing binaries…"
    if (Get-Service -Name $Service -ErrorAction SilentlyContinue) {
        Stop-Service $Service -Force -ErrorAction SilentlyContinue
        sc.exe delete $Service | Out-Null
    }
    Remove-Item -Recurse -Force $InstallDir -ErrorAction SilentlyContinue
    Write-Host "[aii-install] uninstalled (data dir at $DataDir kept)"
    exit 0
}

# Elevation check
$wid = [Security.Principal.WindowsIdentity]::GetCurrent()
$prp = New-Object Security.Principal.WindowsPrincipal($wid)
if (-not $prp.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Error "Re-run from an Administrator PowerShell."
    exit 1
}

# Rust toolchain
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "[aii-install] downloading rustup-init.exe…"
    $rustupExe = "$env:TEMP\rustup-init.exe"
    Invoke-WebRequest "https://win.rustup.rs/x86_64" -OutFile $rustupExe
    & $rustupExe -y --default-toolchain stable
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
}

$BuildDir = "$env:TEMP\aii-build-$(Get-Random)"
New-Item -ItemType Directory -Path $BuildDir | Out-Null
Write-Host "[aii-install] cloning $SrcRepo into $BuildDir…"
git clone --depth 1 $SrcRepo "$BuildDir\aii"
Push-Location "$BuildDir\aii"

Write-Host "[aii-install] cargo build --release (takes 3–10 min)…"
cargo build --release -p aii-node -p aii-cli -p aii-mcp

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
New-Item -ItemType Directory -Force -Path "$DataDir\data" | Out-Null
Copy-Item "target\release\aiid.exe"    "$InstallDir\aiid.exe"    -Force
Copy-Item "target\release\aii.exe"     "$InstallDir\aii.exe"     -Force
Copy-Item "target\release\aii-mcp.exe" "$InstallDir\aii-mcp.exe" -Force
Pop-Location

$HereGenesis = Join-Path $PSScriptRoot "config\testnet-genesis.json"
Copy-Item $HereGenesis "$DataDir\genesis.json" -Force

# Validator keystore
if ((-not $Observer) -and (-not (Test-Path "$DataDir\keystore.json"))) {
    Write-Host "[aii-install] generating validator keystore at $DataDir\keystore.json"
    & "$InstallDir\aii.exe" validator keygen | Out-File "$DataDir\keystore.json" -Encoding ASCII
}

$ArgsObs = @(
    "--data-dir", "$DataDir\data",
    "--rpc",      "0.0.0.0:8545",
    "--testnet",
    "--bootnode", $Bootnode
)
$ArgsVal = $ArgsObs + @(
    "--bft",
    "--genesis", "$DataDir\genesis.json",
    "--keystore","$DataDir\keystore.json",
    "--bft-listen","0.0.0.0:30311",
    "--discovery-seeds", $DiscoverySeeds,
    "--peers",   "8.211.135.234:30311,106.14.223.128:30311"
)
if ($DiscoveryAdvertise) {
    $ArgsVal += @("--discovery-advertise", $DiscoveryAdvertise)
}
if ($BftAdvertise) {
    $ArgsVal += @("--bft-advertise", $BftAdvertise)
}
$ExecArgs = if ($Observer) { $ArgsObs } else { $ArgsVal }

$BinPath  = "`"$InstallDir\aiid.exe`" " + (($ExecArgs | ForEach-Object { "`"$_`"" }) -join " ")

if (Get-Service -Name $Service -ErrorAction SilentlyContinue) {
    Stop-Service $Service -Force -ErrorAction SilentlyContinue
    sc.exe delete $Service | Out-Null
    Start-Sleep -Seconds 1
}

sc.exe create $Service binPath= "$BinPath" start= auto DisplayName= "AII Chain Node" | Out-Null
sc.exe description $Service "AII blockchain node (validator + RPC). Logs: $LogFile" | Out-Null
Start-Service $Service

Start-Sleep -Seconds 3
$svc = Get-Service -Name $Service
if ($svc.Status -eq "Running") {
    Write-Host "[aii-install] ✅ aiid is running. Logs: $LogFile"
    Write-Host "[aii-install] RPC:    http://127.0.0.1:8545"
    Write-Host "[aii-install] CLI:    & '$InstallDir\aii.exe' status --rpc http://127.0.0.1:8545"
} else {
    Write-Error "[aii-install] aiid service failed to start; check $LogFile"
    exit 1
}

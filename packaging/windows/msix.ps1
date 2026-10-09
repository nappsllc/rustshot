# Build dist\rustshot-<version>-x64.msix for the Microsoft Store (unsigned; the
# Store signs it). Identity (Partner Center > Product identity) comes from env:
#   MSIX_IDENTITY_NAME, MSIX_PUBLISHER (CN=...), MSIX_PUBLISHER_DISPLAY
# Optional sideload copy: MSIX_SIGN_PFX (base64 .pfx whose subject equals
# MSIX_PUBLISHER) + MSIX_SIGN_PASSWORD -> dist\rustshot-<version>-x64-sideload.msix
$ErrorActionPreference = 'Stop'
Set-Location (Resolve-Path (Join-Path $PSScriptRoot '..\..'))

$v = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
if ($v -notmatch '^\d+\.\d+\.\d+$') { throw "MSIX needs a plain x.y.z version, got '$v'" }
$exe = 'target\release\rustshot.exe'
if (-not (Test-Path $exe)) { throw "$exe missing (cargo build --release first)" }

function Or($a, $b) { if ($a) { $a } else { $b } }
$name = Or $env:MSIX_IDENTITY_NAME 'nappsllc.rustshot.dev'
$pub = Or $env:MSIX_PUBLISHER 'CN=nappsllc-dev'
$disp = Or $env:MSIX_PUBLISHER_DISPLAY 'nappsllc'
$esc = { param($s) [System.Security.SecurityElement]::Escape($s) }

$stage = 'dist\stage-msix'
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force "$stage\Assets" | Out-Null
Copy-Item $exe $stage
Copy-Item LICENSE "$stage\LICENSE.txt"
Copy-Item THIRD_PARTY_NOTICES.md $stage
Copy-Item packaging\icons\msix\*.png "$stage\Assets"
$manifest = (Get-Content packaging\windows\AppxManifest.xml.in -Raw).
    Replace('@IDENTITY_NAME@', (& $esc $name)).
    Replace('@PUBLISHER@', (& $esc $pub)).
    Replace('@PUBLISHER_DISPLAY@', (& $esc $disp)).
    Replace('@VERSION@', "$v.0")
Set-Content -Path "$stage\AppxManifest.xml" -Value $manifest -Encoding utf8

$bin = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.*\x64\makeappx.exe" |
    Sort-Object FullName -Descending | Select-Object -First 1
if (-not $bin) { throw 'makeappx.exe not found (install the Windows 10/11 SDK)' }
$sdk = $bin.DirectoryName

$out = "dist\rustshot-$v-x64.msix"
& "$sdk\makeappx.exe" pack /o /d $stage /p $out
if ($LASTEXITCODE) { throw "makeappx failed ($LASTEXITCODE)" }
Write-Host "wrote $out"

if ($env:MSIX_SIGN_PFX) {
    $pfx = Join-Path ([System.IO.Path]::GetTempPath()) 'rustshot-sideload.pfx'
    [System.IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:MSIX_SIGN_PFX))
    $signed = "dist\rustshot-$v-x64-sideload.msix"
    Copy-Item $out $signed -Force
    & "$sdk\signtool.exe" sign /fd SHA256 /f $pfx /p $env:MSIX_SIGN_PASSWORD $signed
    $rc = $LASTEXITCODE
    Remove-Item $pfx -Force
    if ($rc) { throw "signtool failed ($rc)" }
    Write-Host "wrote $signed"
}

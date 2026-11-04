# Builds buku.exe and the installer, signing both when a certificate is configured.
#
#   powershell -ExecutionPolicy Bypass -File installer\build.ps1
#
# Signing (see docs/SIGNING.md) is switched on by one of:
#   BUKU_SIGN_CERT  SHA-1 thumbprint of a code-signing certificate in the
#                   current user's store (a hardware token shows up there too)
#   BUKU_SIGNTOOL   a full sign command for Inno Setup, with $f for the file,
#                   for anything else (Azure Trusted Signing, another tool)
# With neither set, the build is unsigned, as before.
#
# When signing was requested, the build fails unless the shipped installer's
# Authenticode status is Valid. For testing with a certificate Windows does not
# trust, set BUKU_SIGN_ALLOW_UNTRUSTED=1: the installer then only has to be signed.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$timestamp = 'https://timestamp.digicert.com'

function Find-SignTool {
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    # Only directories named like 10.0.22621.0 count; [version] sorts them numerically.
    $found = Get-ChildItem -Path $kits -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.Directory.Name -eq 'x64' -and $_.Directory.Parent.Name -match '^\d+(\.\d+){1,3}$' } |
        Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
        Select-Object -First 1
    if (-not $found) { throw 'signtool.exe not found; install the Windows SDK.' }
    $found.FullName
}

function Find-Iscc {
    foreach ($dir in @("$env:LOCALAPPDATA\Programs\Inno Setup 6", "${env:ProgramFiles(x86)}\Inno Setup 6")) {
        $iscc = Join-Path $dir 'ISCC.exe'
        if (Test-Path $iscc) { return $iscc }
    }
    throw 'ISCC.exe not found; install Inno Setup 6.'
}

$signCommand = $env:BUKU_SIGNTOOL
if (-not $signCommand -and $env:BUKU_SIGN_CERT) {
    $thumbprint = $env:BUKU_SIGN_CERT -replace '\s', ''
    if ($thumbprint -notmatch '^[0-9A-Fa-f]{40}$') {
        throw 'BUKU_SIGN_CERT must be a SHA-1 thumbprint: exactly 40 hex digits (spaces are ignored).'
    }
    $signtool = Find-SignTool
    $signCommand = "`"$signtool`" sign /sha1 $thumbprint /fd sha256 /tr $timestamp /td sha256 `$f"
}

Push-Location $root
try {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed.' }

    $iscc = Find-Iscc
    $isccArgs = @('/Q')
    if ($signCommand) {
        # Inno Setup signs buku.exe, the uninstaller and the installer itself.
        $isccArgs += '/DSIGN'
        $isccArgs += "/Sbuku=$($signCommand -replace '"', '$q')"
        Write-Host 'Signing is on.'
    } else {
        Write-Host 'No certificate configured: building unsigned.'
    }
    $isccArgs += 'installer\buku.iss'
    & $iscc @isccArgs
    if ($LASTEXITCODE -ne 0) { throw 'ISCC failed.' }

    # Check only the installer this build produced, not leftovers from older versions.
    $version = Select-String -Path 'installer\buku.iss' -Pattern '^#define AppVersion "([^"]+)"' |
        Select-Object -First 1 | ForEach-Object { $_.Matches[0].Groups[1].Value }
    if (-not $version) { throw 'AppVersion not found in installer\buku.iss.' }
    $installer = Get-Item "target\installer\BUKU-Setup-$version.exe"
    $signature = Get-AuthenticodeSignature $installer.FullName
    Write-Host ("{0}: {1}" -f $installer.Name, $signature.Status)
    if ($signCommand) {
        if ($env:BUKU_SIGN_ALLOW_UNTRUSTED -eq '1') {
            if (-not $signature.SignerCertificate) { throw "$($installer.Name) is not signed." }
        } elseif ($signature.Status -ne 'Valid') {
            throw "$($installer.Name) signature status is $($signature.Status), expected Valid. Set BUKU_SIGN_ALLOW_UNTRUSTED=1 to accept an untrusted test certificate."
        }
    }
    Get-Item 'target\installer\*.exe' | ForEach-Object { Write-Host $_.FullName }
} finally {
    Pop-Location
}

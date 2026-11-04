# Signing the installer

An unsigned installer works, but Windows SmartScreen warns "Windows protected
your PC" and shows the publisher as unknown. Signing puts a verified publisher
name on `buku.exe`, the uninstaller and the installer, and lets SmartScreen
build a reputation for them.

The build is ready for it; what it needs is a code-signing certificate.

## Getting a certificate

Public CAs only issue code-signing keys on hardware (a USB token or a cloud
HSM), so a certificate is no longer a file you download. The usual routes:

- **A cloud signing service** such as Microsoft's Azure Artifact Signing
  (formerly Trusted Signing): a monthly fee, identity checked by Microsoft,
  no token to keep.
- **An OV certificate from a CA** (for example Certum, Sectigo, SSL.com),
  delivered on a token or through the CA's cloud signing. Certum also has a
  lower-priced certificate for open-source developers.

Check current prices and eligibility with the provider; they change. Note
that no certificate makes the SmartScreen warning disappear on day one: the
reputation builds up as people download and run the signed installer.

A self-signed certificate is not a substitute. Other computers don't trust it,
so the warning stays.

## Building signed

The build script signs when one of these is set, and builds unsigned otherwise.

**Certificate in the Windows store or on a token** — set its SHA-1 thumbprint:

```powershell
$env:BUKU_SIGN_CERT = "0123456789ABCDEF0123456789ABCDEF01234567"
powershell -ExecutionPolicy Bypass -File installer\build.ps1
```

The thumbprint must be exactly 40 hex digits (spaces are ignored); anything
else stops the build before it starts. The script finds the newest
`signtool.exe` in the Windows SDK and signs with SHA-256 and a DigiCert
timestamp (over HTTPS), so signatures stay valid after the certificate expires.

**Any other signer** (a cloud service's own tool, a different timestamp server)
— give the full command, with `$f` where the file name goes:

```powershell
$env:BUKU_SIGNTOOL = '"C:\path\to\signtool.exe" sign /dlib "C:\path\Azure.CodeSigning.Dlib.dll" /dmdf "C:\path\metadata.json" /fd sha256 /tr http://timestamp.acs.microsoft.com /td sha256 $f'
powershell -ExecutionPolicy Bypass -File installer\build.ps1
```

The script prints the installer's signature status at the end. `Valid`
means signed with a trusted certificate.

When signing was requested, the build fails unless the installer it just
produced (`target\installer\BUKU-Setup-<version>.exe`, with the version taken
from `installer\buku.iss`) has the status `Valid`. To test the pipeline with a
certificate Windows does not trust, such as a self-signed one, opt in:

```powershell
$env:BUKU_SIGN_ALLOW_UNTRUSTED = "1"
```

The build then only requires that the installer carries a signature. Don't set
it for a release build.

## What gets signed

`installer\buku.iss` turns signing on when built with `/DSIGN`: Inno Setup then
signs `buku.exe` as it packs it, the uninstaller (`SignedUninstaller=yes`) and
the finished `BUKU-Setup-<version>.exe`. The copy in `target\release` stays
unsigned; only what the installer ships is signed.

# Code Signing Policy

## Signing identity

Releases of RSWebTWAIN are signed using a code signing certificate provided by
the [SignPath Foundation](https://signpath.org/) under their free open-source
program. The certificate is held and managed by SignPath and is used only to
sign release artifacts produced by the official RSWebTWAIN build pipeline.

## What is signed

Every published GitHub Release includes a signed Windows MSI installer. The MSI
and the executables it contains (`scan-agent.exe`,
`twain-scanner-32bit-x86_64-pc-windows-msvc.exe`) carry an Authenticode
signature with an RFC 3161 timestamp.

## Build provenance

All signed artifacts are built by GitHub Actions from a tagged commit on the
`main` branch of <https://github.com/seancampbell3161/RSWebTWAIN>. Tagged
commits matching `v*` trigger `.github/workflows/build-msi.yml`, which is the
only workflow authorized to request a signature. No artifact built outside this
pipeline is ever signed.

## Signing-team roles

- **Author / Reviewer / Approver:** Sean Campbell (GitHub
  [`seancampbell3161`](https://github.com/seancampbell3161))

All roles require GitHub multi-factor authentication. Each release is approved
manually by creating a signed `v*` git tag on the protected `main` branch.

## File metadata

Signed binaries carry the following Authenticode attributes:

- **Product name:** `RSWebTWAIN`
- **Publisher:** `SignPath Foundation` (signing on behalf of the RSWebTWAIN
  project)
- **Version:** matches the released git tag

## Reporting concerns

If you believe a signed artifact has been tampered with or signed without
authorization, please open an issue at
<https://github.com/seancampbell3161/RSWebTWAIN/issues> or email
<sean.campbell3161@gmail.com>.

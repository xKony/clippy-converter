# Packaging

`clippy-converter.wxs` is a WiX v3 source for a per-user Windows MSI:

- Installs to `%LocalAppData%\Programs\ClippyConverter` (no admin rights).
- Creates a Start Menu shortcut to the installed exe.
- Major upgrades (and same-version reinstalls) replace cleanly; `UpgradeCode`
  `{B62287E1-114A-4DD4-81BB-42F1531373AF}` must never change.
- MSI version comes from Cargo.toml `[package].version`, passed by
  `.github/workflows/release.yml` as `-dVersion`.
- Run-at-login is owned by the app itself (HKCU Run key) — intentionally not
  duplicated in the installer.

Build locally (WiX 3.14 layout):

```powershell
cargo build --release --locked
& "$env:WIX\bin\candle.exe" -arch x64 "-dVersion=0.0.0" `
    "-dExePath=$PWD\target\release\clippy-converter.exe" -out obj\ packaging/clippy-converter.wxs
& "$env:WIX\bin\light.exe" -out ClippyConverter-0.0.0-x64.msi obj\clippy-converter.wixobj
```

CI uses the WiX Toolset preinstalled on `windows-latest`; no extra setup step.

# Launcher artwork

- `t7patch-logo.png`: 512 x 512 PNG, transparent outside the rounded orange tile.
- `t7patch-icon.png`: 256 x 256 PNG embedded in the launcher's Windows icons.
- `t7patch.ico`: Windows icon with 16, 24, 32, 48, 64, 128 and 256 px images.

The artwork reproduces the launcher's `rs` monogram: Segoe UI Semibold,
oxide orange `#CE723C`, charcoal `#151719` and rounded corners. It is project
artwork inspired by the launcher, not the official Rust language logo.

Regenerate on Windows with Segoe UI Semibold installed:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\generate-logo.ps1
```

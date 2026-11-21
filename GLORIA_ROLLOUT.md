# GloriaSetup rollout

Oculus is a Tauri 2 app. A tag like `v1.2.0` now builds `oculus.exe`, packs it with `projectGloria/GloriaSetup@v0.1.0`
and attaches `Oculus_1.2.0_x64-setup.exe` to that tag's release (created with generated notes if missing).

## Please

- Add the repository secrets `WINDOWS_PFX_BASE64` and `WINDOWS_PFX_PASSWORD`. Without them the setup is unsigned.
- Version mismatch: project files say `0.1.0`, the launcher catalog says `0.2.0`. `gloria.toml` uses `0.1.0` (project
  files). The packed version comes from the tag, so tag `vX` while `package.json` etc. still say `0.1.0` produces a setup
  named `X` whose app reports `0.1.0`. Run `npm run set:version -- <X>` before tagging.
- Data folders: I listed `%APPDATA%\dev.gloria.oculus` (settings, from the Tauri identifier),
  `%LOCALAPPDATA%\dev.gloria.oculus` (WebView2 data) and `%LOCALAPPDATA%\Oculus` (logs). Remove any you do not want
  deleted on "remove my data". Recordings and screenshots go to the user's output folder and are never touched.
- `legacy_names = ["Oculus"]`: earlier tags shipped a Tauri NSIS installer whose DisplayName is the productName `Oculus`.
- The old `release` job and the NSIS bundle step were removed from `build-installers.yml`; the exe is built with
  `--no-bundle`. `ci.yml` still builds the NSIS installer as a build check only.
- Icon: no `Oculus.ico` in the root, so `src-tauri/icons/icon.ico` (the app's own icon) is used.
- Only `oculus.exe` is staged: the shutter sound is embedded with `include_bytes!` and the frontend is embedded by Tauri.
- Nothing was built or run locally; the workflow is untested.

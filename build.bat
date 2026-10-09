@echo off
setlocal
cd /d "%~dp0"

rem Workaround: Tauri CLI --ci flag only accepts true/false, but CI=1 breaks it
set CI=true

rem Fast dev build (NOT production):
rem  - --debug: uses cargo dev profile, skips release LTO / opt-level=3 / codegen-units=1 / strip (biggest speedup)
rem  - --no-bundle: skips NSIS/MSI bundling entirely, just produces target\debug\openleash.exe (bundling is the slow part)
rem  - --config tauri.fast.conf.json: overrides beforeBuildCommand to "npx vite build", skipping slow `tsc` typecheck
call npx tauri build --debug --no-bundle --ci --config src-tauri/tauri.fast.conf.json
if errorlevel 1 (
  echo Build failed.
  exit /b 1
)

if not exist "compiled" mkdir "compiled"

rem Copy portable exe to ./compiled (no installers by design for speed)
rem NOTE: --debug outputs to target\debug, not target\release
if exist "src-tauri\target\debug\openleash.exe" copy /y "src-tauri\target\debug\openleash.exe" "compiled\"

echo.
echo Done. Artifacts in .\compiled\
dir "compiled"

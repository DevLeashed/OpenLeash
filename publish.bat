@echo off

setlocal EnableExtensions EnableDelayedExpansion
rem ===========================================================================
rem  publish.bat - release binaries for every platform OpenLeash ships on.
rem
rem  Output: .\published\
rem
rem  Windows targets ship BOTH an installer and a portable executable:
rem
rem    openleash-windows-amd64-setup.exe   x86_64 Windows  (NSIS installer)
rem    openleash-windows-amd64.exe         x86_64 Windows  (portable)
rem    openleash-windows-arm64-setup.exe   ARM64 Windows  (NSIS installer)
rem    openleash-windows-arm64.exe         ARM64 Windows  (portable)
rem    openleash-linux-amd64               x86_64 Linux  (AppImage)
rem    openleash-linux-arm64               aarch64 Linux (raw ELF)
rem    openleash-macos-arm64               Apple Silicon  (see publish-macos.sh)
rem
rem  The installer is the primary Windows artifact: it is what gives a user an
rem  uninstall path and a Start Menu entry. The portable .exe is kept alongside
rem  it because it is the only artifact that can be verified by checksum without
rem  running installer code, and because a signed bare .exe is still useful.
rem
rem  First Windows build of a machine needs NETWORK ACCESS: tauri-bundler does
rem  not use a system NSIS, it downloads its own toolchain (makensis + stubs +
rem  plugins) to %LOCALAPPDATA%\tauri\NSIS on first use and reuses it after
rem  that. If that download fails the build is retried portable-only rather than
rem  failing outright, so a release is never blocked by it.
rem
rem  ---------------------------------------------------------------------------
rem  THERE IS NO "DEBIAN BUILD" AND NO "FEDORA BUILD"
rem
rem  A Linux binary is only as portable as the glibc it was linked against.
rem  Build it on an older distro and it runs everywhere; build it on a newer one
rem  (Fedora) and it demands a newer glibc, which is exactly why distributions
rem  refuse to install it on anything older. So "Debian" and "Fedora" are not
rem  two builds - they are the same build, and the one that targets the OLDEST
rem  system wins. Tauri v2 needs WebKitGTK 4.1, which rules out the ancient
rem  distros, so the baseline is Debian 12 (see docker\build-linux.sh).
rem
rem  The one Linux binary per arch below therefore runs on Debian, Ubuntu,
rem  Fedora, Arch and ARM boards alike. Do not "fix" this by pointing the
rem  container at Fedora; that only makes the binary work on fewer machines.
rem  If you later want real .deb and .rpm packages, that is a packaging
rem  decision on top of this binary, not a second compile.
rem
rem  ---------------------------------------------------------------------------
rem  WHAT A WINDOWS MACHINE CANNOT BUILD
rem
rem    macOS ARM64  Apple binaries need the macOS SDK and Apple's codesign.
rem                 Neither is redistributable and neither runs on Windows, so
rem                 no cross-compiler exists. Run publish-macos.sh on a Mac.
rem    Linux        Needs Docker. Start Docker Desktop first.
rem    Windows ARM  Needs the MSVC ARM64 tools:
rem                 VS Installer > Modify > Individual components >
rem                 "C++ ARM64 build tools for Windows".
rem
rem  Targets that cannot be built here are reported as SKIPPED with the reason,
rem  and everything that did build is still published. Run with "strict" as the
rem  first argument to make any skip a non-zero exit code.
rem ===========================================================================

rem --ci only parses a real boolean; CI=1 is not one.
set "CI=true"

set "ROOT=%~dp0"
cd /d "%ROOT%"

set "OUT=published"
set "NAME=openleash"
set "STRICT="
if /i "%~1"=="strict" set "STRICT=1"

rem Each status var is "built" or starts with SKIPPED / FAILED.
set "B_WIN64=not attempted"
set "B_ARM64=not attempted"
set "B_LIN64=not attempted"
set "B_LINARM=not attempted"
set "W64_NSIS=not attempted"
set "WARM_NSIS=not attempted"
set "W64_WHY="
set "WARM_WHY="

if not exist "%OUT%" mkdir "%OUT%"

echo.
echo ============================================================
echo  OpenLeash publish
echo ============================================================
echo.

rem ---------------------------------------------------------------------------
rem 1. Windows x86_64
rem ---------------------------------------------------------------------------
echo [1/4] Windows x86_64 ...
call :rust_target x86_64-pc-windows-msvc
if errorlevel 1 goto :win64_failed
call :build_win x86_64-pc-windows-msvc "%NAME%-windows-amd64.exe"
if errorlevel 1 goto :win64_failed
call :publish "src-tauri\target\x86_64-pc-windows-msvc\release\%NAME%.exe" "%OUT%\%NAME%-windows-amd64.exe"
if errorlevel 1 goto :win64_failed
set "B_WIN64=built"
echo       OK  %OUT%\%NAME%-windows-amd64.exe
call :nsis_status "%NAME%-windows-amd64-setup.exe" W64_NSIS W64_WHY
goto :next_arm

:win64_failed
set "B_WIN64=FAILED (build or copy failed - see output above)"
echo       !! this build FAILED

rem ---------------------------------------------------------------------------
rem 2. Windows ARM64
rem
rem Emits an installer AND a bare .exe. The NSIS installer is an x86 program
rem that runs on the ARM machine under emulation and carries the native ARM64
rem binary inside it - that is Tauri-supported, and it is still the only one of
rem the two that leaves the user an uninstall path.
rem ---------------------------------------------------------------------------
:next_arm
echo.
echo [2/4] Windows ARM64 ...
call :arm64_env
if errorlevel 1 goto :arm64_skip
call :rust_target aarch64-pc-windows-msvc
if errorlevel 1 goto :arm64_failed
call :build_win aarch64-pc-windows-msvc "%NAME%-windows-arm64.exe"
if errorlevel 1 goto :arm64_failed
call :publish "src-tauri\target\aarch64-pc-windows-msvc\release\%NAME%.exe" "%OUT%\%NAME%-windows-arm64.exe"
if errorlevel 1 goto :arm64_failed
set "B_ARM64=built"
echo       OK  %OUT%\%NAME%-windows-arm64.exe
call :nsis_status "%NAME%-windows-arm64-setup.exe" WARM_NSIS WARM_WHY
goto :next_linux

:arm64_skip
set "B_ARM64=SKIPPED (MSVC ARM64 build tools not installed)"
echo       SKIP: MSVC ARM64 build tools are not installed.
echo       VS Installer - Modify - Individual components
echo       - tick "C++ ARM64 build tools for Windows", then re-run.
goto :next_linux

:arm64_failed
set "B_ARM64=FAILED (build or copy failed - see output above)"
echo       !! this build FAILED

rem ---------------------------------------------------------------------------
rem 3 + 4. Linux x86_64 and aarch64
rem
rem x64  -> AppImage: one self-contained file, runs with no install step.
rem arm  -> raw ELF. linuxdeploy has no ARM build, so Tauri cannot produce an
rem          ARM AppImage on an x86 host; the cross-compiled raw binary is the
rem          artifact that actually works. It needs webkit2gtk on the target
rem          machine (one apt / dnf install), which the AppImage does not.
rem ---------------------------------------------------------------------------
:next_linux
echo.
echo [3/4] Linux x86_64 (AppImage) ...
call :container_engine
if errorlevel 1 goto :linux64_skip
call :build_linux x86_64-unknown-linux-gnu appimage "%NAME%-linux-amd64"
if errorlevel 1 goto :linux64_failed
set "B_LIN64=built"
echo       OK  %OUT%\%NAME%-linux-amd64
goto :next_linuxarm

:linux64_skip
set "B_LIN64=SKIPPED (no running container engine)"
echo       SKIP: no running container engine. Start Docker Desktop, re-run.
goto :next_linuxarm

:linux64_failed
set "B_LIN64=FAILED (see output above)"
echo       !! this build FAILED

:next_linuxarm
echo.
echo [4/4] Linux aarch64 (raw ELF) ...
if not defined ENGINE (
  set "B_LINARM=SKIPPED (no running container engine)"
  echo       SKIP: no running container engine.
  goto :summary
)
call :build_linux aarch64-unknown-linux-gnu raw "%NAME%-linux-arm64"
if errorlevel 1 goto :linuxarm_failed
set "B_LINARM=built"
echo       OK  %OUT%\%NAME%-linux-arm64
goto :summary

:linuxarm_failed
set "B_LINARM=FAILED (see output above)"
echo       !! this build FAILED

rem ===========================================================================
rem  Summary
rem ===========================================================================
:summary
echo.
echo ============================================================
echo  %OUT%\
echo ============================================================
set "ANY=0"
for %%f in ("%OUT%\*") do set /a ANY=1
if "!ANY!"=="1" (
  for %%f in ("%OUT%\*") do echo    %%~nxf    %%~zf bytes
) else (
  echo    (empty)
)

echo.
echo   windows-amd64    !B_WIN64!   installer: !W64_NSIS!
echo   windows-arm64    !B_ARM64!   installer: !WARM_NSIS!
echo   linux-amd64      !B_LIN64!
echo   linux-arm64      !B_LINARM!
echo   macos-arm64      NOT BUILT HERE - Apple binaries cannot be cross-compiled.
echo.
echo   macOS build it yourself:
echo     1. clone this repo on a Mac
echo     2. xcode-select --install
echo     3. ./publish-macos.sh
echo     It writes published\%NAME%-macos-arm64 - copy that one file back here.
echo.

rem Strict mode has to actually look at the statuses. It used to exit 1
rem unconditionally, which was only accidentally right: macOS always reports NOT
rem BUILT HERE, so every run had a skip and an all-clean run would still have
rem failed.
rem
rem No `exit` inside a nested parenthesised block: cmd silently drops the exit code
rem when the outer block is still open, which turned strict mode into a no-op that
rem always reported success. A chained `if` is the form that actually propagates.
set "BAD="
for %%v in ("!B_WIN64!" "!B_ARM64!" "!B_LIN64!" "!B_LINARM!") do (
  if defined STRICT echo   %%~v
  if /i not "%%~v"=="built" set "BAD=1"
)
if defined BAD if defined STRICT exit /b 1
if defined STRICT echo   strict mode: every target built clean.
exit /b 0


rem ===========================================================================
rem  Subroutines
rem ===========================================================================

rem :build_win <triple> <portableDestName>
rem
rem Builds the Windows target and publishes its NSIS installer alongside the
rem portable executable (which the CALLER publishes, since it needs to copy the
rem bare target\release\openleash.exe rather than the bundler's output).
rem
rem The installer is best-effort. A bundling failure - most plausibly the
rem first-run NSIS download being blocked by a firewall - must not cost us the
rem portable build, so we retry with --no-bundle and carry on. A COMPILE failure
rem still returns 1, so a caller can never mistake a broken build for a missing
rem installer.
rem
rem Only `--bundles nsis` is requested, never the config default of "all": "all"
rem also tries MSI, which needs the Windows VBSCRIPT optional feature and fails
rem on a host without it.
rem
rem tauri-bundler names the installer {productName}_{version}_{arch}-setup.exe, so
rem it is found by glob rather than by an exact name. `dir /o-n` sorts newest
rem first and the `if not defined` guard keeps only that one: a STALE installer
rem left in the target dir by an earlier build would otherwise be picked up and
rem published as if it were current. The published name is then derived from
rem <portableDestName> so it stays openleash-windows-amd64-setup.exe whatever
rem productName happens to be.
rem
rem W64_NSIS / WARM_NSIS (and their _WHY companions) are what the caller reports,
rem so they are set from the triple here rather than returned: batch can only
rem return an errorlevel, and the summary needs a reason string, not just a bit.
rem
rem The NSIS toolchain is downloaded on first use to %LOCALAPPDATA%\tauri\NSIS
rem (tauri-bundler vendors its own makensis + stubs + plugins rather than using a
rem system NSIS), so the first Windows build on a machine needs network access.
rem
rem `goto :eof` is used inside the glob loop because `break` is not a batch
rem keyword.
rem
rem :build_win <triple> <portableDestName>
:build_win
set "WTRIPLE=%~1"
set "WPORTABLE=%~2"
if /i "%WTRIPLE%"=="aarch64-pc-windows-msvc" (
  set "WNSIS_VAR=WARM_NSIS"
  set "WWHY_VAR=WARM_WHY"
) else (
  set "WNSIS_VAR=W64_NSIS"
  set "WWHY_VAR=W64_WHY"
)

rem Mark the installer pending before the build, so an aborted run cannot leave
rem a stale "built" from a previous invocation on screen. The "=" must sit
rem directly against the variable NAME: `set "!V!=x"` is the only form that works
rem once !V! has been expanded, because cmd strips the first "=" as the set
rem operator and leaves the second as the separator. Every indirect set in this
rem file needs that doubled "=" for exactly that reason.
set "W64_NSIS=SKIPPED (not attempted)"
set "WARM_NSIS=SKIPPED (not attempted)"
set "!WNSIS_VAR!=building..."
set "!WWHY_VAR!="

call npx tauri build --target %WTRIPLE% --bundles nsis --ci
if errorlevel 1 (
  echo       !! build FAILED
  echo       retrying portable-only, in case this was only a bundling problem ...
  call npx tauri build --target %WTRIPLE% --no-bundle --ci
  if errorlevel 1 (
    set "!WNSIS_VAR!=SKIPPED (build failed)"
    set "!WWHY_VAR!=the build failed outright"
    exit /b 1
  )
  set "W64_WHY=NSIS bundling failed - portable exe only"
  set "WARM_WHY=NSIS bundling failed - portable exe only"
  exit /b 0
)

set "BNAME="
for /f "delims=" %%f in ('dir /b /o-n "src-tauri\target\%WTRIPLE%\release\bundle\nsis\*-setup.exe" 2^>nul') do (
  if not defined BNAME set "BNAME=%%f"
)
if not defined BNAME (
  echo       !! tauri reported success but produced no .exe under bundle\nsis
  set "!WNSIS_VAR!=SKIPPED (no installer produced)"
  set "!WWHY_VAR!=tauri produced no bundle\nsis installer"
  exit /b 0
)

set "BSTEM=%WPORTABLE:.exe=%"
call :publish "src-tauri\target\%WTRIPLE%\release\bundle\nsis\!BNAME!" "!OUT%\!BSTEM!-setup.exe"
if errorlevel 1 (
  set "!WNSIS_VAR!=SKIPPED (copy into published\ failed)"
  set "!WWHY_VAR!=installer built but could not be copied into published\"
  exit /b 0
)
set "!WNSIS_VAR!=built"
set "!WWHY_VAR!="
exit /b 0

rem :nsis_status <expectedInstallerName> <statusVar> <whyVar>
rem
rem Echoes the installer's fate after a successful target build and fills in a
rem SKIPPED reason when the status is anything other than "built". It exists as a
rem subroutine only because the x64 and arm64 call sites are otherwise identical
rem blocks. The sets below use the `%~n` form with a literal value in the same
rem statement, which needs none of the doubled-"=" care that `set "!VAR!v"`
rem requires - see the note in :build_win.
rem
rem It checks that the file really is in published\ rather than trusting the
rem status string, so a "built" status with no file behind it is reported as a
rem skip instead of printing a success line for a missing artifact.
rem
rem :nsis_status <expectedInstallerName> <statusVar> <whyVar>
:nsis_status
if exist "%OUT%\%~1" if "!%~2!"=="built" (
  echo       OK  %OUT%\%~1
  exit /b 0
)
set "%~2=SKIPPED"
if not defined %~3 set "%~3=the installer was not produced"
set "%~3=!%~3!"
exit /b 0

rem :publish <source> <dest>   copy, then strip the .exe extension is NOT done
rem   on purpose: the file keeps whatever name the user asked for.
:publish
copy /y "%~1" "%~2" >nul
if errorlevel 1 (
  echo       copy failed: %~1
  exit /b 1
)
exit /b 0

rem :rust_target <triple>
:rust_target
rustup target list --installed 2>nul | findstr /x /c:"%~1" >nul
if not errorlevel 1 exit /b 0
echo       adding rust target %~1
rustup target add %~1
if errorlevel 1 exit /b 1
exit /b 0

rem :arm64_env  -  put the ARM64 cross toolchain in the environment.
rem vcvarsall x64_arm64 is the supported way to do this; the arm64 folders under
rem HostArm64/ are for building ON arm64, not from it. Returns 1 if unavailable.
:arm64_env
if defined VSCMD_ARG_TGT_ARCH (
  if /i "%VSCMD_ARG_TGT_ARCH%"=="arm64" exit /b 0
)
rem Find the VS install that has the ARM64 tools.
rem Deliberately NOT done with for /f + a backtick command: cmd re-parses that
rem form and strips the quotes off a path containing "(x86)", failing with
rem     'C:\Program' is not recognized as an internal or external command
rem Redirect to a file and read it back, which preserves the quoting.
set "VSPATH="
set "VSPROBE=%TEMP%~nx0.vswhere.txt"
"%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products "*" -requires Microsoft.VisualStudio.Component.VC.Tools.ARM64 -property installationPath > "%VSPROBE%" 2>nul
set /p VSPATH=<"%VSPROBE%"
del /q "%VSPROBE%" >nul 2>&1
if not defined VSPATH exit /b 1
if not exist "%VSPATH%\VC\Auxiliary\Build\vcvarsall.bat" exit /b 1
call "%VSPATH%\VC\Auxiliary\Build\vcvarsall.bat" x64_arm64 >nul
if errorlevel 1 exit /b 1
exit /b 0

rem :container_engine  -  set ENGINE=docker|podman, or fail.
:container_engine
set "ENGINE="
docker info >nul 2>&1 && set "ENGINE=docker"
if not defined ENGINE (
  podman info >nul 2>&1 && set "ENGINE=podman"
)
if not defined ENGINE exit /b 1
exit /b 0

rem :build_linux <triple> <appimage|raw> <destName>
:build_linux
set "TRIPLE=%~1"
set "MODE=%~2"
set "DEST=%OUT%\%~3"
set "MOUNT=%CD:\=/%"
set "PLATFORM=linux/amd64"
if /i "%TRIPLE%"=="aarch64-unknown-linux-gnu" set "PLATFORM=linux/arm64"
rem arm64 runs under QEMU rather than cross-compiling: Debian cannot supply
rem arm64 WebKitGTK dev packages (see docker\build-linux.sh for why).
echo       building in a Debian 12 container on %PLATFORM%
echo       full release build: lto, one codegen unit, no debug symbols.
%ENGINE% run --rm ^
  --platform %PLATFORM% ^
  -v "%MOUNT%":/app ^
  -v "%ROOT%docker:/build:ro" ^
  -w /app ^
  -e CI=true ^
  -e DEBIAN_FRONTEND=noninteractive ^
  node:20-bookworm ^
  /bin/bash /build/build-linux.sh %TRIPLE% %MODE%
if errorlevel 1 exit /b 1

if /i "%MODE%"=="appimage" (
  for /f "delims=" %%f in ('dir /b /o-n "src-tauri\target\%TRIPLE%\release\bundle\appimage\*.AppImage" 2^>nul') do (
    call :publish "src-tauri\target\%TRIPLE%\release\bundle\appimage\%%f" "!DEST!"
    goto :eof
  )
  echo       no .AppImage was produced
  exit /b 1
)

call :publish "src-tauri\target\%TRIPLE%\release\%NAME%" "!DEST!"
exit /b %errorlevel%
@echo off
rem ===========================================================================
rem  DAUx Plugin Platform - build script (Windows / x64)
rem
rem  Builds: Core + DAUxHost + DAUxScan + C++ example (CMake), the Rust wrapper
rem  + example (cargo), and the .NET wrapper + Avalonia example (NativeAOT),
rem  then assembles the .NET bundle into <root>\build\plugins.
rem
rem  Usage:  build.cmd [all|core|rust|dotnet]    (default: all)
rem ===========================================================================
setlocal enableextensions

set "SCRIPT_DIR=%~dp0"
set "DAUX_DIR=%SCRIPT_DIR%.."
set "ROOT_DIR=%SCRIPT_DIR%..\.."
set "BUILD_DIR=%ROOT_DIR%\build"
set "PLUGINS_DIR=%BUILD_DIR%\plugins"
set "CONFIG=Release"
set "RID=win-x64"

set "WHAT=%~1"
if "%WHAT%"=="" set "WHAT=all"

rem Make vswhere reachable so NativeAOT can locate the MSVC linker.
set "PATH=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer;%PATH%"

if not exist "%PLUGINS_DIR%" mkdir "%PLUGINS_DIR%"

if /I "%WHAT%"=="all"    goto :core
if /I "%WHAT%"=="core"   goto :core
if /I "%WHAT%"=="rust"   goto :rust
if /I "%WHAT%"=="dotnet" goto :dotnet
echo Unknown component "%WHAT%". Use: all ^| core ^| rust ^| dotnet
exit /b 2

rem ---------------------------------------------------------------------------
:core
echo === [1/3] Core + Host + C++ example (CMake) ===
cmake -S "%DAUX_DIR%" -B "%BUILD_DIR%" -A x64 || goto :error
cmake --build "%BUILD_DIR%" --config %CONFIG% || goto :error
if /I "%WHAT%"=="core" goto :scan

rem ---------------------------------------------------------------------------
:rust
echo === [2/3] Rust wrapper + example (cargo) ===
pushd "%DAUX_DIR%\examples\gain-rust-gpui" || goto :error
cargo build --release || (popd & goto :error)
popd
copy /Y "%DAUX_DIR%\examples\gain-rust-gpui\target\release\gain_rust_gpui.dll" ^
        "%PLUGINS_DIR%\daux_gain_rust.dauxplug" >nul || goto :error
echo     -^> %PLUGINS_DIR%\daux_gain_rust.dauxplug
if /I "%WHAT%"=="rust" goto :scan

rem ---------------------------------------------------------------------------
:dotnet
echo === [3/3] .NET wrapper + Avalonia example (NativeAOT bundle) ===
set "PLUGIN_PROJ=%DAUX_DIR%\examples\gain-dotnet-avalonia\Plugin\gain-dotnet-avalonia.csproj"
dotnet publish "%PLUGIN_PROJ%" -r %RID% -c %CONFIG% || goto :error

set "PUB=%DAUX_DIR%\examples\gain-dotnet-avalonia\Plugin\bin\%CONFIG%\net8.0\%RID%\publish"
set "BUNDLE=%PLUGINS_DIR%\daux_gain_dotnet.dauxplug"

if exist "%BUNDLE%" rmdir /S /Q "%BUNDLE%"
mkdir "%BUNDLE%\Exec"      2>nul
mkdir "%BUNDLE%\Library"   2>nul
mkdir "%BUNDLE%\Resources" 2>nul

copy /Y "%PUB%\gain-dotnet-avalonia.dll" "%BUNDLE%\Exec\daux_gain_dotnet.dll" >nul || goto :error
rem All other shared libs (Skia, HarfBuzz, GLES, ...) go into Library/.
copy /Y "%PUB%\*.dll" "%BUNDLE%\Library\" >nul
del /Q "%BUNDLE%\Library\gain-dotnet-avalonia.dll" 2>nul
copy /Y "%DAUX_DIR%\examples\gain-dotnet-avalonia\manifest.xml" "%BUNDLE%\manifest.xml" >nul || goto :error
echo     -^> %BUNDLE% (bundle)

rem ---------------------------------------------------------------------------
:scan
echo === Verifying with DAUxScan ===
set "SCAN=%BUILD_DIR%\bin\%CONFIG%\DAUxScan.exe"
if exist "%SCAN%" (
    "%SCAN%" "%PLUGINS_DIR%"
) else (
    echo (DAUxScan not built; run with "core" or "all" first.)
)
echo.
echo Build complete. Plugins in: %PLUGINS_DIR%
exit /b 0

:error
echo.
echo *** BUILD FAILED (errorlevel %errorlevel%) ***
exit /b 1

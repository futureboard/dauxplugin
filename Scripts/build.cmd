@echo off
rem ===========================================================================
rem  DAUx Plugin Platform - build script (Windows / x64)
rem
rem  Usage:  build.cmd [all|core|rust|dotnet]    (default: all)
rem ===========================================================================
setlocal enableextensions

set "SCRIPT_DIR=%~dp0"
set "DAUX_DIR=%SCRIPT_DIR%.."
set "ROOT_DIR=%SCRIPT_DIR%.."

set "OS_NAME=windows"
if /I "%PROCESSOR_ARCHITECTURE%"=="AMD64" (
    set "ARCH_NAME=x64"
) else if /I "%PROCESSOR_ARCHITECTURE%"=="ARM64" (
    set "ARCH_NAME=arm64"
) else (
    set "ARCH_NAME=%PROCESSOR_ARCHITECTURE%"
)

set "BUILD_DIR=%ROOT_DIR%\build.%OS_NAME%.%ARCH_NAME%"
set "PLUGINS_DIR=%BUILD_DIR%\plugins"
set "CONFIG=Release"
set "RID=win-%ARCH_NAME%"

set "WHAT=%~1"
if "%WHAT%"=="" set "WHAT=all"

set "PATH=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer;%PATH%"

if not exist "%PLUGINS_DIR%" mkdir "%PLUGINS_DIR%"

if /I "%WHAT%"=="all"    goto :core
if /I "%WHAT%"=="core"   goto :core
if /I "%WHAT%"=="rust"   goto :rust
if /I "%WHAT%"=="dotnet" goto :dotnet
echo Unknown component "%WHAT%". Use: all ^| core ^| rust ^| dotnet
exit /b 2

:core
echo === [1/3] Core + Host + C++ example (CMake) ===
cmake -S "%DAUX_DIR%" -B "%BUILD_DIR%" -A %ARCH_NAME% || goto :error
cmake --build "%BUILD_DIR%" --config %CONFIG% || goto :error
if /I "%WHAT%"=="core" goto :scan

:rust
echo === [2/3] Rust wrapper + example (cargo) ===
pushd "%DAUX_DIR%\Examples\GainRustGpui" || goto :error
cargo build --release || (popd & goto :error)
popd
copy /Y "%DAUX_DIR%\Examples\GainRustGpui\target\release\gain_rust_gpui.dll" ^
        "%PLUGINS_DIR%\daux_gain_rust.dauxplug" >nul || goto :error
echo     -^> %PLUGINS_DIR%\daux_gain_rust.dauxplug
if /I "%WHAT%"=="rust" goto :scan

:dotnet
echo === [3/3] .NET wrapper + Avalonia example (NativeAOT bundle) ===
set "PLUGIN_PROJ=%DAUX_DIR%\Examples\GainDotnetAvalonia\Plugin\gain-dotnet-avalonia.csproj"
dotnet publish "%PLUGIN_PROJ%" -r %RID% -c %CONFIG% || goto :error

set "PUB=%DAUX_DIR%\Examples\GainDotnetAvalonia\Plugin\bin\%CONFIG%\net8.0\%RID%\publish"
set "BUNDLE=%PLUGINS_DIR%\daux_gain_dotnet.dauxplug"

if exist "%BUNDLE%" rmdir /S /Q "%BUNDLE%"
mkdir "%BUNDLE%\Exec"      2>nul
mkdir "%BUNDLE%\Library"   2>nul
mkdir "%BUNDLE%\Resources" 2>nul

copy /Y "%PUB%\gain-dotnet-avalonia.dll" "%BUNDLE%\Exec\daux_gain_dotnet.dll" >nul || goto :error
copy /Y "%PUB%\*.dll" "%BUNDLE%\Library\" >nul
del /Q "%BUNDLE%\Library\gain-dotnet-avalonia.dll" 2>nul
copy /Y "%DAUX_DIR%\Examples\GainDotnetAvalonia\manifest.xml" "%BUNDLE%\manifest.xml" >nul || goto :error
echo     -^> %BUNDLE% (bundle)

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

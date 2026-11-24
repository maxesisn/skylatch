@echo off
rem SPDX-License-Identifier: Apache-2.0
setlocal
set "SKIP_RUST="
if /i "%~1"=="--skip-rust" (
    set "SKIP_RUST=1"
    shift /1
)
if not "%~1"=="" (
    echo Usage: build.bat [--skip-rust] 1>&2
    exit /b 2
)
cd /d "%~dp0" || exit /b 1
for %%I in ("%~dp0..\..") do set "REPO=%%~fI"
set "RUST_OUT=%REPO%\target\release"
set "CORE_DLL=skylatch_client_core.dll"

set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" (
    echo vswhere.exe not found. Install Visual Studio or Build Tools with Desktop development with C++. 1>&2
    exit /b 1
)
set "VCVARS64="
for /f "usebackq delims=" %%I in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find VC\Auxiliary\Build\vcvars64.bat`) do set "VCVARS64=%%I"
if not defined VCVARS64 (
    echo vcvars64.bat not found. Install the MSVC x64 tools and a Windows SDK. 1>&2
    exit /b 1
)
call "%VCVARS64%"
if errorlevel 1 exit /b %ERRORLEVEL%

if not defined SKIP_RUST (
    if defined CARGO_HOME set "PATH=%CARGO_HOME%\bin;%PATH%"
    if not defined CARGO_HOME set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
    cargo build --release -p skylatch-client-core --manifest-path "%REPO%\Cargo.toml" --target-dir "%REPO%\target"
    if errorlevel 1 exit /b 1
)

for %%I in ("%RUST_OUT%\%CORE_DLL%" "%RUST_OUT%\%CORE_DLL%.lib") do if not exist "%%~I" (
    echo Missing %%~I. Run build.bat without --skip-rust using x86_64-pc-windows-msvc Rust. 1>&2
    exit /b 1
)
rc.exe /nologo /fo skylatch.res skylatch.rc
if errorlevel 1 exit /b %ERRORLEVEL%
cl.exe /nologo /std:c++20 /O2 /EHsc /W3 /MT /utf-8 /DUNICODE /D_UNICODE /DNOMINMAX /DWIN32_LEAN_AND_MEAN /D_WIN32_WINNT=0x0A00 ^
  /I"..\..\crates\skylatch-client-core\include" client.cpp /Fe:skylatch-client.exe ^
  /link /SUBSYSTEM:WINDOWS /ENTRY:wmainCRTStartup skylatch.res "%RUST_OUT%\%CORE_DLL%.lib" ^
  d3d11.lib dxgi.lib mfplat.lib mfuuid.lib mf.lib user32.lib gdi32.lib ole32.lib oleaut32.lib imm32.lib wmcodecdspuuid.lib runtimeobject.lib d2d1.lib dwrite.lib
if errorlevel 1 exit /b %ERRORLEVEL%
copy /Y "%RUST_OUT%\%CORE_DLL%" "%~dp0%CORE_DLL%"
exit /b %ERRORLEVEL%

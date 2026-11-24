@echo off
rem SPDX-License-Identifier: Apache-2.0
setlocal
rem Pass a staging directory with the five Skylatch binaries. Runtime DLLs
rem collected from MSYS2, if needed, belong under its runtime subdirectory.
if "%~3"=="" (
    echo Usage: build-installer.bat BIN_DIR OUTPUT_DIR VERSION [OUTPUT_BASENAME] 1>&2
    exit /b 2
)
set "ISCC=iscc.exe"
where iscc.exe >nul 2>&1
if errorlevel 1 set "ISCC=%ProgramFiles(x86)%\Inno Setup 6\ISCC.exe"
set "OUTPUT_NAME=%~4"
if not defined OUTPUT_NAME set "OUTPUT_NAME=Skylatch-Setup-x64"
"%ISCC%" "/DBinDir=%~f1" "/DAppVersion=%~3" "/O%~f2" "/F%OUTPUT_NAME%" "%~dp0skylatch.iss"
exit /b %ERRORLEVEL%

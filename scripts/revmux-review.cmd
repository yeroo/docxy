@echo off
rem Bridge for ralphex: pkg/executor/custom.go runs the configured script via
rem exec.Command with NO shell, and Windows cannot exec a .sh directly, so the
rem real logic lives in revmux-review.sh and this hands it to Git bash.
setlocal
set "BASHEXE=C:\Program Files\Git\usr\bin\bash.exe"
if not exist "%BASHEXE%" set "BASHEXE=C:\Program Files\Git\bin\bash.exe"
if not exist "%BASHEXE%" (
    echo error: Git bash not found; revmux review hook cannot run 1>&2
    exit /b 1
)
rem Invoking bash.exe directly does NOT bring Git's coreutils along: the script
rem would find git and revmux on the system PATH but die on `date: command not
rem found`. Put the directory holding bash.exe on PATH so its siblings resolve.
for %%I in ("%BASHEXE%") do set "BASHDIR=%%~dpI"
set "PATH=%BASHDIR%;%PATH%"
"%BASHEXE%" "%~dp0revmux-review.sh" %*

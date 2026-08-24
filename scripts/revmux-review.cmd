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
"%BASHEXE%" "%~dp0revmux-review.sh" %*

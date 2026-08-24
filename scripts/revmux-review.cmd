@echo off
REM Windows entry point for the ralphex -> revmux review bridge.
REM ralphex runs the configured review script with exec.Command(script, promptFile),
REM which on Windows cannot execute a .sh directly - hence this wrapper.
REM %~dp0 is this file's directory, so the pair stays relocatable.
REM
REM Git\bin\bash.exe is the MSYS wrapper and sets up PATH, so date/sed/cp resolve.
REM Git\usr\bin\bash.exe is the raw binary and does not - reaching for it first
REM gives "date: command not found" inside an otherwise working script.
setlocal
set "BASH=%ProgramFiles%\Git\bin\bash.exe"
if not exist "%BASH%" set "BASH=%ProgramFiles%\Git\usr\bin\bash.exe"
if not exist "%BASH%" (
  echo revmux-review: git bash not found; install Git for Windows or edit this wrapper 1>&2
  echo ^<^<^<RALPHEX:CODEX_REVIEW_DONE^>^>^>
  exit /b 0
)
"%BASH%" "%~dp0revmux-review.sh" %1
exit /b 0

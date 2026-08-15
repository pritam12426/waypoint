@echo off
rem waypointd Windows service installer (drives NSSM).
rem
rem Prereqs:
rem   1. Copy waypointd.exe to C:\waypoint\waypointd.exe
rem   2. Install NSSM (https://nssm.cc/download) and put nssm.exe on PATH
rem   3. Run this file once from an *elevated* Command Prompt:
rem        cd deploy\windows
rem        install-waypointd.bat
rem
rem Service management afterwards:
rem   nssm start|stop|restart waypointd
rem   nssm status waypointd
rem   nssm remove waypointd confirm

setlocal

set "BIN=C:\waypoint\waypointd.exe"
set "DATA=C:\waypoint\data"
set "BACKUP=C:\waypoint\backups"
set "CACHE=C:\waypoint\cache"

if not exist "%BIN%" (
  echo [ERROR] waypointd.exe not found at %BIN% — copy the binary there first.
  exit /b 1
)

rem ---- Edit these before running -------------------------------------------
set "SERVE_TOKEN=CHANGE_ME_long_random_string"
set "SERVE_HOST=localhost"
set "SERVE_PORT=8080"
rem ---------------------------------------------------------------------------

echo Creating data directories...
if not exist "%DATA%"  mkdir "%DATA%"
if not exist "%BACKUP%" mkdir "%BACKUP%"
if not exist "%CACHE%"  mkdir "%CACHE%"

echo Installing service...
nssm install waypointd "%BIN%"
if errorlevel 1 exit /b 1

nssm set waypointd AppDirectory "C:\waypoint"
nssm set waypointd Start SERVICE_AUTO_START

nssm set waypointd AppEnvironmentExtra ^
  WAYPOINTD_DB_FILE="%DATA%\waypoint.sqlite" ^
  WAYPOINTD_SERVE_TOKEN=%SERVE_TOKEN% ^
  WAYPOINTD_SERVE_HOST=%SERVE_HOST% ^
  WAYPOINTD_SERVE_PORT=%SERVE_PORT% ^
  WAYPOINTD_LOG_LEVEL=info ^
  WAYPOINTD_LOG_FORMAT=human-readable ^
  WAYPOINTD_BACKUP_DIR="%BACKUP%" ^
  WAYPOINTD_BACKUP_INTERVAL_SECS=86400 ^
  WAYPOINTD_BACKUP_KEEP=7 ^
  WAYPOINTD_CACHE_DIR="%CACHE%"

rem waypointd logs to stderr by default; NSSM redirects it here.
nssm set waypointd AppStderr "%DATA%\waypointd.err.log"
nssm set waypointd AppStdout "%DATA%\waypointd.out.log"
nssm set waypointd AppRotateFiles 1
nssm set waypointd AppRotateBytes 10485760
nssm set waypointd AppRestartDelay 5000

echo Starting service...
nssm start waypointd
if errorlevel 1 (
  echo [ERROR] The service did not start. Check the log files in %DATA%.
  exit /b 1
)

echo.
echo waypointd is running as a Windows service. Open http://localhost:8080
echo and paste your WAYPOINTD_SERVE_TOKEN in Settings.
endlocal

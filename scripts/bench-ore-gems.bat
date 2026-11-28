@echo off
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0bench-ore-gems.ps1" %*
exit /b %errorlevel%

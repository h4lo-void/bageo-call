@echo off
rem Myna - double-click to start (Windows). See start.ps1 for what it does.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0start.ps1" %*
if errorlevel 1 pause

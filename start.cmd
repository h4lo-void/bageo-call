@echo off
rem SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
rem SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

rem Bageo - double-click to start (Windows). See start.ps1 for what it does.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0start.ps1" %*
if errorlevel 1 pause

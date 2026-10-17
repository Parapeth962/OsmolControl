@echo off
rem Double-click to start OsmolControl. If Windows asks, allow it on Private networks.
cd /d "%~dp0"
bikectl.exe start --trainer-bridge
echo.
echo OsmolControl stopped. Press any key to close this window.
pause >nul

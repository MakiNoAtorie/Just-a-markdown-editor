@echo off
rem Builds the release exe into dist\. Pass -Test -Benchmark for the full check.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build.ps1" %*
if errorlevel 1 (echo Build failed.) else (echo Built: "%~dp0dist\Just a markdown editor.exe")
pause

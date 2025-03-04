@echo off
echo Creating portable version...

REM Створюємо теку для портативної версії
set PORTABLE_DIR=GameTrimmer-portable
if exist %PORTABLE_DIR% rmdir /s /q %PORTABLE_DIR%
mkdir %PORTABLE_DIR%

REM Копіюємо всі файли з білду
xcopy /s /y build\src\Release\*.* %PORTABLE_DIR%\

REM Видаляємо зайві файли та папки
if exist %PORTABLE_DIR%\translations rmdir /s /q %PORTABLE_DIR%\translations

echo Portable version created in %PORTABLE_DIR% directory
echo You can now copy this directory anywhere and run GameTrimmer.exe 
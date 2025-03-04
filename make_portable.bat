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

REM Копіюємо необхідні системні DLL
echo Copying Visual C++ Runtime libraries...
copy "%SystemRoot%\System32\vcruntime140.dll" %PORTABLE_DIR%\
copy "%SystemRoot%\System32\vcruntime140_1.dll" %PORTABLE_DIR%\
copy "%SystemRoot%\System32\msvcp140.dll" %PORTABLE_DIR%\
copy "%SystemRoot%\System32\msvcp140_1.dll" %PORTABLE_DIR%\
copy "%SystemRoot%\System32\msvcp140_2.dll" %PORTABLE_DIR%\
copy "%SystemRoot%\System32\concrt140.dll" %PORTABLE_DIR%\

echo Portable version created in %PORTABLE_DIR% directory
echo You can now copy this directory anywhere and run GameTrimmer.exe 
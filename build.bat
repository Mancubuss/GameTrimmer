@echo off
echo Checking requirements...

REM Перевіряємо наявність Qt
set QT_DIR=C:\Qt\6.8.2\msvc2022_64
if not exist "%QT_DIR%\bin\windeployqt.exe" (
    echo Error: Qt not found at %QT_DIR%
    exit /b 1
)

REM Перевіряємо наявність CMake
where /q cmake
if errorlevel 1 (
    echo Error: CMake not found in PATH
    exit /b 1
)

echo Building GameTrimmer...

REM Створюємо та переходимо в теку build
if exist build rmdir /s /q build
mkdir build
cd build

REM Генеруємо проект
echo Generating project files...
cmake -G "Visual Studio 17 2022" -A x64 ..

REM Збираємо проект
echo Building project...
cmake --build . --config Release

REM Копіюємо залежності Qt
echo Copying Qt dependencies...
"%QT_DIR%\bin\windeployqt.exe" --no-translations --no-system-d3d-compiler --no-opengl-sw --no-svg --no-network src\Release\GameTrimmer.exe

REM Видаляємо російську локалізацію, якщо вона є
if exist src\Release\translations\qt_ru.qm del src\Release\translations\qt_ru.qm

REM Видаляємо невикористані плагіни та директорії
if exist src\Release\generic rmdir /s /q src\Release\generic
if exist src\Release\iconengines rmdir /s /q src\Release\iconengines
if exist src\Release\imageformats rmdir /s /q src\Release\imageformats
if exist src\Release\networkinformation rmdir /s /q src\Release\networkinformation
if exist src\Release\styles rmdir /s /q src\Release\styles
if exist src\Release\tls rmdir /s /q src\Release\tls

REM Видаляємо невикористані DLL
if exist src\Release\opengl32sw.dll del src\Release\opengl32sw.dll
if exist src\Release\D3Dcompiler_47.dll del src\Release\D3Dcompiler_47.dll
if exist src\Release\Qt6Network.dll del src\Release\Qt6Network.dll
if exist src\Release\Qt6Svg.dll del src\Release\Qt6Svg.dll

echo Build complete!

REM Запускаємо програму
echo Starting GameTrimmer...
start src\Release\GameTrimmer.exe

cd .. 
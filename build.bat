@echo off
chcp 65001 > nul
setlocal

echo Checking requirements...

REM Перевіряємо наявність CMake
where cmake >nul 2>nul
if %ERRORLEVEL% NEQ 0 (
    echo Помилка: CMake не знайдено. Будь ласка, встановіть CMake.
    pause
    exit /b 1
)

REM Перевіряємо наявність Visual Studio
if not exist "%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" (
    echo Помилка: Visual Studio не знайдено. Будь ласка, встановіть Visual Studio 2022.
    pause
    exit /b 1
)

REM Перевіряємо наявність Qt
if not exist "C:\Qt\6.8.2\msvc2022_64\bin\windeployqt.exe" (
    echo Помилка: Qt не знайдено. Будь ласка, встановіть Qt 6.8.2 для MSVC 2022 64-bit.
    pause
    exit /b 1
)

echo Building GameTrimmer...

REM Створюємо теку для збірки якщо її немає
if not exist "build" mkdir build
cd build

REM Генеруємо файли проекту за допомогою CMake
echo Generating project files...
cmake -G "Visual Studio 17 2022" -A x64 ..
if %ERRORLEVEL% NEQ 0 (
    echo Помилка: Не вдалося згенерувати проектні файли.
    cd ..
    pause
    exit /b 1
)

REM Компілюємо проект
echo Building project...
cmake --build . --config Release
if %ERRORLEVEL% NEQ 0 (
    echo Помилка: Не вдалося скомпілювати проект.
    cd ..
    pause
    exit /b 1
)

REM Копіюємо Qt DLL файли
echo Copying Qt dependencies...
"C:\Qt\6.8.2\msvc2022_64\bin\windeployqt.exe" --release "src\Release\GameTrimmer.exe"
if %ERRORLEVEL% NEQ 0 (
    echo Помилка: Не вдалося скопіювати Qt DLL файли.
    cd ..
    pause
    exit /b 1
)

echo Build complete!
cd ..

REM Запускаємо програму
echo Starting GameTrimmer...
start build\src\Release\GameTrimmer.exe 
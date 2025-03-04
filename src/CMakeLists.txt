# Визначаємо файли проекту
set(PROJECT_SOURCES
    main.cpp
    mainwindow.cpp
    mainwindow.h
    mainwindow.ui
    steamlibrary.cpp
    steamlibrary.h
    resources.qrc
    GameTrimmer.rc
)

# Створюємо виконуваний файл
add_executable(GameTrimmer WIN32
    ${PROJECT_SOURCES}
)

# Додаємо include директорії
target_include_directories(GameTrimmer PRIVATE ${CMAKE_CURRENT_SOURCE_DIR})

# Знаходимо тільки необхідні компоненти Qt
find_package(Qt6 COMPONENTS Widgets REQUIRED)

# Лінкуємо з Qt бібліотеками
target_link_libraries(GameTrimmer PRIVATE Qt6::Widgets)

# Встановлюємо властивості для MSVC
if(MSVC)
    target_compile_options(GameTrimmer PRIVATE
        /W4     # Рівень попереджень
        /MP     # Багатопроцесорна компіляція
    )
endif()

# Налаштування розгортання Qt
set(CMAKE_INSTALL_SYSTEM_RUNTIME_LIBS_SKIP TRUE)

# Вимикаємо непотрібні плагіни Qt
set(QT_SKIP_PLUGINS 
    "qtuiotouchplugin"
    "qsvgicon"
    "qgif"
    "qico"
    "qjpeg"
    "qsvg"
    "qnetworklistmanager"
    "qmodernwindowsstyle"
    "qcertonlybackend"
    "qschannelbackend"
)

# Вимикаємо непотрібні переклади
set(QT_TRANSLATIONS_TO_SKIP "qt_ru.qm")

# Налаштування для windeployqt
set(CMAKE_WINDEPLOYQT_ARGS 
    --no-translations
    --no-system-d3d-compiler
    --no-opengl-sw
    --no-svg
    --no-network
) 
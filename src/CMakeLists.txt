# Визначаємо файли проекту
set(PROJECT_SOURCES
    main.cpp
    mainwindow.cpp
    mainwindow.h
    mainwindow.ui
    steamlibrary.cpp
    steamlibrary.h
)

# Створюємо виконуваний файл
add_executable(GameTrimmer WIN32
    ${PROJECT_SOURCES}
)

# Додаємо include директорії
target_include_directories(GameTrimmer PRIVATE ${CMAKE_CURRENT_SOURCE_DIR} ${Qt6Widgets_INCLUDE_DIRS} ${Qt6Core_INCLUDE_DIRS})

# Лінкуємо з Qt бібліотеками
target_link_libraries(GameTrimmer PRIVATE
    Qt6::Widgets
    Qt6::Core
)

# Встановлюємо властивості для MSVC
if(MSVC)
    target_compile_options(GameTrimmer PRIVATE
        /W4     # Рівень попереджень
        /MP     # Багатопроцесорна компіляція
    )
endif() 
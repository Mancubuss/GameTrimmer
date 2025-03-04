cmake_minimum_required(VERSION 3.16)
project(GameTrimmer VERSION 0.1 LANGUAGES CXX)

# Налаштування компілятора
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# Налаштування для MSVC
if(MSVC)
    add_compile_options(/Zc:__cplusplus)
endif()

# Вказуємо шлях до Qt
set(CMAKE_PREFIX_PATH "C:/Qt/6.8.2/msvc2022_64")

# Знаходимо Qt пакети
find_package(Qt6 REQUIRED COMPONENTS 
    Widgets 
    Core
)

# Налаштування Qt
set(CMAKE_AUTOMOC ON)
set(CMAKE_AUTORCC ON)
set(CMAKE_AUTOUIC ON)

# Додаємо підтеку з вихідним кодом
add_subdirectory(src) 
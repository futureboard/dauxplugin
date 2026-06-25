# DAUxHost.exe - out-of-process plugin host / scanner.

cmake_minimum_required(VERSION 3.20)
project(dauxhost LANGUAGES CXX)

add_executable(DAUxHost
    src/main.cpp
    src/host_runtime.cpp
    src/plugin_loader.cpp
    src/plugin_editor.cpp
    src/ipc_server.cpp
    src/plugin_scan.cpp)

target_link_libraries(DAUxHost PRIVATE daux::core)
target_compile_features(DAUxHost PRIVATE cxx_std_17)

if(MSVC)
    target_compile_definitions(DAUxHost PRIVATE _CRT_SECURE_NO_WARNINGS UNICODE _UNICODE)
endif()

set_target_properties(DAUxHost PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin")

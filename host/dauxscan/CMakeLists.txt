# DAUxScan - standalone scanner. Reuses the loader/scan/metadata sources from
# DAUxHost (no editor/IPC machinery) plus daux_core.

cmake_minimum_required(VERSION 3.20)
project(dauxscan LANGUAGES CXX)

set(DAUXHOST_SRC ${CMAKE_CURRENT_SOURCE_DIR}/../dauxhost/src)

add_executable(DAUxScan
    src/main.cpp
    ${DAUXHOST_SRC}/plugin_scan.cpp
    ${DAUXHOST_SRC}/plugin_loader.cpp)

target_include_directories(DAUxScan PRIVATE ${DAUXHOST_SRC})
target_link_libraries(DAUxScan PRIVATE daux::core)
target_compile_features(DAUxScan PRIVATE cxx_std_17)

if(MSVC)
    target_compile_definitions(DAUxScan PRIVATE _CRT_SECURE_NO_WARNINGS UNICODE _UNICODE)
endif()

set_target_properties(DAUxScan PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin")

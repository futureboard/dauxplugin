# gain-cpp-headless - reference C++ DAUx plugin.
#
# Builds a shared library and renames the artifact to "*.dauxplug" so the host
# loader sees the canonical plugin extension.

cmake_minimum_required(VERSION 3.20)
project(daux_example_gain_cpp LANGUAGES CXX)

add_library(gain_cpp MODULE gain.cpp)
target_link_libraries(gain_cpp PRIVATE daux::abi)
target_compile_features(gain_cpp PRIVATE cxx_std_17)

# This module IS a plugin: export the entry symbol.
target_compile_definitions(gain_cpp PRIVATE DAUX_PLUGIN_BUILD)

if(MSVC)
    target_compile_definitions(gain_cpp PRIVATE _CRT_SECURE_NO_WARNINGS)
endif()

# .dauxplug extension, no "lib" prefix, predictable output dir.
set_target_properties(gain_cpp PROPERTIES
    PREFIX ""
    OUTPUT_NAME "daux_gain_cpp"
    SUFFIX ".dauxplug"
    LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/plugins"
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/plugins")

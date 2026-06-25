# DAUx Plugin Platform - top-level build.
#
# Builds the C++ pieces: the Core ABI + host-side helper library, DAUxHost.exe,
# DAUxScan, and the C++ example plugin. The Rust and .NET wrappers build with
# their own toolchains (cargo / dotnet) and are not part of this CMake tree.
#
#   cmake -S daux -B build -A x64
#   cmake --build build --config Release
#
# Artifacts (multi-config generators put these under <config>/):
#   build/bin/DAUxHost.exe, build/bin/DAUxScan.exe
#   build/plugins/daux_gain_cpp.dauxplug

cmake_minimum_required(VERSION 3.20)
project(daux LANGUAGES C CXX)

if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
    set(CMAKE_BUILD_TYPE Release CACHE STRING "" FORCE)
endif()

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

add_subdirectory(Core)
add_subdirectory(Host/DauxHost)
add_subdirectory(Host/DauxScan)
add_subdirectory(Examples/GainCppHeadless)

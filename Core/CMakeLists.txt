# DAUx Plugin Core
#
# Produces:
#   - daux_abi   : INTERFACE target exposing the public C ABI headers only.
#   - daux_core  : STATIC C++ host-side helper library.

cmake_minimum_required(VERSION 3.20)
project(daux_core LANGUAGES C CXX)

set(DAUX_CORE_INCLUDE_DIR ${CMAKE_CURRENT_SOURCE_DIR}/include)

# --- public ABI: headers only -------------------------------------------------
add_library(daux_abi INTERFACE)
add_library(daux::abi ALIAS daux_abi)
target_include_directories(daux_abi INTERFACE
    $<BUILD_INTERFACE:${DAUX_CORE_INCLUDE_DIR}>
    $<INSTALL_INTERFACE:include>)
target_compile_features(daux_abi INTERFACE c_std_11)

# --- host-side C++ helper library ---------------------------------------------
file(GLOB_RECURSE DAUX_CORE_SOURCES CONFIGURE_DEPENDS
    "${CMAKE_CURRENT_SOURCE_DIR}/src/*.cpp")

add_library(daux_core STATIC ${DAUX_CORE_SOURCES})
add_library(daux::core ALIAS daux_core)

target_include_directories(daux_core PUBLIC
    $<BUILD_INTERFACE:${DAUX_CORE_INCLUDE_DIR}>
    $<INSTALL_INTERFACE:include>)
target_compile_features(daux_core PUBLIC cxx_std_17)
set_target_properties(daux_core PROPERTIES POSITION_INDEPENDENT_CODE ON)

if(UNIX)
    target_link_libraries(daux_core PUBLIC ${CMAKE_DL_LIBS})
endif()

if(MSVC)
    target_compile_definitions(daux_core PRIVATE _CRT_SECURE_NO_WARNINGS UNICODE _UNICODE)
endif()

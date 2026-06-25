/*
 * daux_export.h - Symbol visibility / export macros for the DAUx Plugin ABI.
 *
 * Part of "DAUx Plugin Core". This header is intentionally tiny and dependency
 * free so it can be included from every other public header.
 *
 * Rules (see API Design Rules in the project spec):
 *   - Public boundary is a pure C ABI.
 *   - No STL / Rust / .NET types ever cross this boundary.
 */
#ifndef DAUX_EXPORT_H
#define DAUX_EXPORT_H

/* ---- calling convention -------------------------------------------------- */
/* We pin the calling convention so wrappers in other languages (Rust/.NET)
 * can declare matching signatures deterministically. cdecl on all platforms. */
#if defined(_WIN32)
#  define DAUX_CALL __cdecl
#else
#  define DAUX_CALL
#endif

/* ---- import/export ------------------------------------------------------- */
/* A plugin (.dauxplug) defines DAUX_PLUGIN_BUILD when compiling itself so its
 * entry point is exported. A host leaves it undefined and the entry symbol is
 * resolved dynamically (GetProcAddress / dlsym), so no import attribute is
 * strictly required - but we still provide the macro for completeness. */
#if defined(_WIN32)
#  if defined(DAUX_PLUGIN_BUILD)
#    define DAUX_EXPORT __declspec(dllexport)
#  else
#    define DAUX_EXPORT __declspec(dllimport)
#  endif
#else
#  if defined(DAUX_PLUGIN_BUILD)
#    define DAUX_EXPORT __attribute__((visibility("default")))
#  else
#    define DAUX_EXPORT
#  endif
#endif

#ifdef __cplusplus
#  define DAUX_EXTERN_C extern "C"
#else
#  define DAUX_EXTERN_C
#endif

#define DAUX_API DAUX_EXTERN_C DAUX_EXPORT

#endif /* DAUX_EXPORT_H */

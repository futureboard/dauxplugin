/*
 * DAUx/Abi/Export.h - Symbol visibility / export macros for the DAUx Plugin ABI.
 */
#ifndef DAUX_ABI_EXPORT_H
#define DAUX_ABI_EXPORT_H

#if defined(_WIN32)
#  define DAUX_CALL __cdecl
#else
#  define DAUX_CALL
#endif

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

#endif /* DAUX_ABI_EXPORT_H */

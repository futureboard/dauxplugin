/*
 * DAUx/Abi/Types.h - Core POD typedefs and fixed-string sizes.
 */
#ifndef DAUX_ABI_TYPES_H
#define DAUX_ABI_TYPES_H

#include <stdint.h>
#include <stddef.h>

#include <DAUx/Abi/Export.h>

#ifdef __cplusplus
extern "C" {
#endif

#define DAUX_ID_SIZE     64
#define DAUX_NAME_SIZE   128
#define DAUX_SHORT_SIZE  32
#define DAUX_VENDOR_SIZE 128

typedef int32_t daux_bool;
#define DAUX_TRUE  1
#define DAUX_FALSE 0

typedef void* daux_plugin_handle;
typedef void* daux_editor_handle;
typedef void* daux_host_context;
typedef void* daux_native_window;

typedef uint32_t daux_param_id;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_ABI_TYPES_H */

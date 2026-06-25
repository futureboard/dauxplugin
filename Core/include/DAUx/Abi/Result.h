/*
 * DAUx/Abi/Result.h - Result codes for the DAUx Plugin ABI.
 */
#ifndef DAUX_ABI_RESULT_H
#define DAUX_ABI_RESULT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef int32_t daux_result;

enum {
    DAUX_OK = 0,

    /* Legacy error codes (stable; do not reorder). */
    DAUX_ERR_UNKNOWN          = -1,
    DAUX_ERR_INVALID_ARG      = -2,
    DAUX_ERR_NOT_SUPPORTED    = -3,
    DAUX_ERR_NOT_INITIALIZED  = -4,
    DAUX_ERR_OUT_OF_MEMORY    = -5,
    DAUX_ERR_INVALID_STATE    = -6,
    DAUX_ERR_BUFFER_TOO_SMALL = -7,
    DAUX_ERR_ABI_MISMATCH     = -8,
    DAUX_ERR_NOT_FOUND        = -9,
    DAUX_ERR_IO               = -10,

    /* Expanded names (aliases for documentation / new code). */
    DAUX_ERROR_UNKNOWN           = DAUX_ERR_UNKNOWN,
    DAUX_ERROR_INVALID_ARGUMENT  = DAUX_ERR_INVALID_ARG,
    DAUX_ERROR_INVALID_STATE     = DAUX_ERR_INVALID_STATE,
    DAUX_ERROR_UNSUPPORTED       = DAUX_ERR_NOT_SUPPORTED,
    DAUX_ERROR_NOT_FOUND         = DAUX_ERR_NOT_FOUND,
    DAUX_ERROR_OUT_OF_MEMORY     = DAUX_ERR_OUT_OF_MEMORY,
    DAUX_ERROR_PLUGIN_FAILED     = -11,
    DAUX_ERROR_HOST_FAILED       = -12
};

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_ABI_RESULT_H */

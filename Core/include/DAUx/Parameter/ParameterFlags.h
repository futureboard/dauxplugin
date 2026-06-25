/*
 * DAUx/Parameter/ParameterFlags.h
 */
#ifndef DAUX_PARAMETER_PARAMETER_FLAGS_H
#define DAUX_PARAMETER_PARAMETER_FLAGS_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef int32_t daux_param_flags;
enum {
    DAUX_PARAM_NONE        = 0,
    DAUX_PARAM_AUTOMATABLE = 1 << 0,
    DAUX_PARAM_STEPPED     = 1 << 1,
    DAUX_PARAM_READ_ONLY   = 1 << 2,
    DAUX_PARAM_IS_BYPASS   = 1 << 3
};

enum {
    DAUX_PARAMETER_FLAG_AUTOMATABLE  = 1 << 0,
    DAUX_PARAMETER_FLAG_READONLY     = 1 << 1,
    DAUX_PARAMETER_FLAG_BOOLEAN      = 1 << 2,
    DAUX_PARAMETER_FLAG_INTEGER      = 1 << 3,
    DAUX_PARAMETER_FLAG_HIDDEN       = 1 << 4,
    DAUX_PARAMETER_FLAG_LOGARITHMIC  = 1 << 5
};

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PARAMETER_PARAMETER_FLAGS_H */

/*
 * DAUx/Parameter/ParameterInfo.h
 */
#ifndef DAUX_PARAMETER_PARAMETER_INFO_H
#define DAUX_PARAMETER_PARAMETER_INFO_H

#include <stdint.h>

#include <DAUx/Abi/Types.h>
#include <DAUx/Parameter/ParameterFlags.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct daux_param_info {
    daux_param_id id;
    char          name[DAUX_NAME_SIZE];
    char          short_name[DAUX_SHORT_SIZE];
    char          unit[DAUX_SHORT_SIZE];

    double        default_value;
    double        min_value;
    double        max_value;
    int32_t       step_count;

    daux_param_flags flags;
    uint32_t      reserved[4];
} daux_param_info;

typedef struct DAUxParameterInfo {
    uint32_t    id;
    const char* name;
    const char* short_name;
    const char* unit;
    double      default_value;
    double      min_value;
    double      max_value;
    uint32_t    flags;
} DAUxParameterInfo;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PARAMETER_PARAMETER_INFO_H */

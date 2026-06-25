#ifndef DAUX_EVENT_PARAMETER_EVENT_H
#define DAUX_EVENT_PARAMETER_EVENT_H

#include <stdint.h>

#include <DAUx/Abi/Types.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxParameterEvent {
    daux_param_id id;
    double        value;
    uint32_t      flags;
} DAUxParameterEvent;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_EVENT_PARAMETER_EVENT_H */

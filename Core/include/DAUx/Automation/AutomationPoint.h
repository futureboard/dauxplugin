#ifndef DAUX_AUTOMATION_AUTOMATION_POINT_H
#define DAUX_AUTOMATION_AUTOMATION_POINT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxAutomationPoint {
    uint32_t sample_offset;
    double   value;
} DAUxAutomationPoint;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_AUTOMATION_AUTOMATION_POINT_H */

#ifndef DAUX_AUTOMATION_PARAMETER_CHANGE_QUEUE_H
#define DAUX_AUTOMATION_PARAMETER_CHANGE_QUEUE_H

#include <stdint.h>

#include <DAUx/Abi/Types.h>
#include <DAUx/Automation/AutomationPoint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxParameterChange {
    daux_param_id         parameter_id;
    DAUxAutomationPoint*  points;
    uint32_t              point_count;
} DAUxParameterChange;

typedef struct DAUxParameterChangeQueue {
    DAUxParameterChange* changes;
    uint32_t             count;
} DAUxParameterChangeQueue;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_AUTOMATION_PARAMETER_CHANGE_QUEUE_H */

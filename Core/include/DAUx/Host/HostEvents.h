/*
 * DAUx/Host/HostEvents.h
 */
#ifndef DAUX_HOST_HOST_EVENTS_H
#define DAUX_HOST_HOST_EVENTS_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum DAUxHostEvent {
    DAUX_HOST_EVENT_PARAMETER_CHANGED       = 0,
    DAUX_HOST_EVENT_STATE_CHANGED           = 1,
    DAUX_HOST_EVENT_EDITOR_RESIZE_REQUESTED = 2,
    DAUX_HOST_EVENT_LOG                     = 3
} DAUxHostEvent;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_HOST_HOST_EVENTS_H */

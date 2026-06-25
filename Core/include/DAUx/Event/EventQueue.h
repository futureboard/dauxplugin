#ifndef DAUX_EVENT_EVENT_QUEUE_H
#define DAUX_EVENT_EVENT_QUEUE_H

#include <stdint.h>

#include <DAUx/Event/Event.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxEventQueue {
    DAUxEvent* events;
    uint32_t   count;
    uint32_t   capacity;
} DAUxEventQueue;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_EVENT_EVENT_QUEUE_H */

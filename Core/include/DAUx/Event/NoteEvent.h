#ifndef DAUX_EVENT_NOTE_EVENT_H
#define DAUX_EVENT_NOTE_EVENT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxNoteEvent {
    int32_t  note;
    int32_t  velocity;
    uint32_t channel;
    double   tuning;
} DAUxNoteEvent;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_EVENT_NOTE_EVENT_H */

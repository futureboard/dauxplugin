#ifndef DAUX_EVENT_MIDI_EVENT_H
#define DAUX_EVENT_MIDI_EVENT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxMidiEvent {
    uint8_t  status;
    uint8_t  data1;
    uint8_t  data2;
    uint8_t  reserved;
    uint32_t channel;
} DAUxMidiEvent;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_EVENT_MIDI_EVENT_H */

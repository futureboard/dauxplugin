#ifndef DAUX_EVENT_EVENT_H
#define DAUX_EVENT_EVENT_H

#include <stdint.h>

#include <DAUx/Event/MidiEvent.h>
#include <DAUx/Event/NoteEvent.h>
#include <DAUx/Event/ParameterEvent.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum DAUxEventType {
    DAUX_EVENT_UNKNOWN        = 0,
    DAUX_EVENT_MIDI           = 1,
    DAUX_EVENT_NOTE_ON        = 2,
    DAUX_EVENT_NOTE_OFF       = 3,
    DAUX_EVENT_NOTE_PRESSURE  = 4,
    DAUX_EVENT_PITCH_BEND     = 5,
    DAUX_EVENT_PARAMETER      = 6,
    DAUX_EVENT_TRANSPORT      = 7
} DAUxEventType;

typedef struct DAUxEvent {
    DAUxEventType type;
    uint32_t      sample_offset;
    uint32_t      flags;
    union {
        DAUxMidiEvent      midi;
        DAUxNoteEvent      note;
        DAUxParameterEvent parameter;
    } data;
} DAUxEvent;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_EVENT_EVENT_H */

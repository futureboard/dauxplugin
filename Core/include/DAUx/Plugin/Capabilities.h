/*
 * DAUx/Plugin/Capabilities.h
 */
#ifndef DAUX_PLUGIN_CAPABILITIES_H
#define DAUX_PLUGIN_CAPABILITIES_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef int32_t daux_plugin_caps;
enum {
    DAUX_CAP_NONE        = 0,
    DAUX_CAP_EDITOR      = 1 << 0,
    DAUX_CAP_MIDI_INPUT  = 1 << 1,
    DAUX_CAP_MIDI_OUTPUT = 1 << 2,
    DAUX_CAP_STATE       = 1 << 3,
    DAUX_CAP_PRESETS     = 1 << 4
};

enum {
    DAUX_PLUGIN_CAP_AUDIO_INPUT    = 1 << 0,
    DAUX_PLUGIN_CAP_AUDIO_OUTPUT   = 1 << 1,
    DAUX_PLUGIN_CAP_MIDI_INPUT     = 1 << 2,
    DAUX_PLUGIN_CAP_MIDI_OUTPUT    = 1 << 3,
    DAUX_PLUGIN_CAP_EDITOR         = 1 << 4,
    DAUX_PLUGIN_CAP_STATE          = 1 << 5,
    DAUX_PLUGIN_CAP_PRESETS        = 1 << 6,
    DAUX_PLUGIN_CAP_SIDECHAIN      = 1 << 7,
    DAUX_PLUGIN_CAP_OFFLINE_RENDER = 1 << 8
};

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PLUGIN_CAPABILITIES_H */

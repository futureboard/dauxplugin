/*
 * DAUx/Plugin/Category.h
 */
#ifndef DAUX_PLUGIN_CATEGORY_H
#define DAUX_PLUGIN_CATEGORY_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef int32_t daux_plugin_category;
enum {
    DAUX_CATEGORY_UNKNOWN    = 0,
    DAUX_CATEGORY_EFFECT     = 1,
    DAUX_CATEGORY_INSTRUMENT = 2,
    DAUX_CATEGORY_MIDI_FX    = 3,
    DAUX_CATEGORY_ANALYZER   = 4
};

typedef enum DAUxPluginCategory {
    DAUX_PLUGIN_CATEGORY_UNKNOWN      = 0,
    DAUX_PLUGIN_CATEGORY_EFFECT       = 1,
    DAUX_PLUGIN_CATEGORY_INSTRUMENT   = 2,
    DAUX_PLUGIN_CATEGORY_MIDI_EFFECT  = 3,
    DAUX_PLUGIN_CATEGORY_ANALYZER     = 4,
    DAUX_PLUGIN_CATEGORY_UTILITY      = 5
} DAUxPluginCategory;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PLUGIN_CATEGORY_H */

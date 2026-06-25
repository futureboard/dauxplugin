/*
 * DAUx/Audio/ChannelLayout.h
 */
#ifndef DAUX_AUDIO_CHANNEL_LAYOUT_H
#define DAUX_AUDIO_CHANNEL_LAYOUT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum DAUxChannelLayout {
    DAUX_CHANNEL_LAYOUT_UNKNOWN      = 0,
    DAUX_CHANNEL_LAYOUT_MONO         = 1,
    DAUX_CHANNEL_LAYOUT_STEREO       = 2,
    DAUX_CHANNEL_LAYOUT_SURROUND_5_1 = 3,
    DAUX_CHANNEL_LAYOUT_SURROUND_7_1 = 4
} DAUxChannelLayout;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_AUDIO_CHANNEL_LAYOUT_H */

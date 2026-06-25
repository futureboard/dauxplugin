/*
 * DAUx/Audio/AudioBus.h
 */
#ifndef DAUX_AUDIO_AUDIO_BUS_H
#define DAUX_AUDIO_AUDIO_BUS_H

#include <stdint.h>

#include <DAUx/Audio/SampleFormat.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxAudioBus {
    void**            channels;
    uint32_t          channel_count;
    uint32_t          frame_count;
    DAUxSampleFormat  sample_format;
} DAUxAudioBus;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_AUDIO_AUDIO_BUS_H */

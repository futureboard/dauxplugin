/*
 * DAUx/Audio/AudioBuffer.h
 */
#ifndef DAUX_AUDIO_AUDIO_BUFFER_H
#define DAUX_AUDIO_AUDIO_BUFFER_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxAudioBufferF32 {
    float**   channels;
    uint32_t  channel_count;
    uint32_t  frame_count;
} DAUxAudioBufferF32;

typedef struct DAUxAudioBufferF64 {
    double**  channels;
    uint32_t  channel_count;
    uint32_t  frame_count;
} DAUxAudioBufferF64;

/* Legacy planar bus buffer (used by daux_process_data). */
typedef struct daux_audio_bus_buffer {
    uint32_t channel_count;
    float**  channels;
    double** channels64;
} daux_audio_bus_buffer;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_AUDIO_AUDIO_BUFFER_H */

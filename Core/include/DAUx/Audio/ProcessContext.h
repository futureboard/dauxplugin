/*
 * DAUx/Audio/ProcessContext.h
 */
#ifndef DAUX_AUDIO_PROCESS_CONTEXT_H
#define DAUX_AUDIO_PROCESS_CONTEXT_H

#include <stdint.h>

#include <DAUx/Abi/Types.h>
#include <DAUx/Audio/SampleFormat.h>
#include <DAUx/Audio/AudioBuffer.h>

#ifdef __cplusplus
extern "C" {
#endif

#define DAUX_PROCESS_FLAG_PLAYING        (1u << 0)
#define DAUX_PROCESS_FLAG_RECORDING      (1u << 1)
#define DAUX_PROCESS_FLAG_LOOPING        (1u << 2)
#define DAUX_PROCESS_FLAG_OFFLINE_RENDER (1u << 3)

typedef struct DAUxProcessContext {
    double   sample_rate;
    uint32_t block_size;
    uint64_t sample_position;
    double   bpm;
    uint32_t time_sig_num;
    uint32_t time_sig_den;
    uint32_t flags;
} DAUxProcessContext;

/* Legacy process context (stable ABI layout). */
typedef struct daux_process_context {
    double   sample_rate;
    uint32_t block_size;
    uint32_t sample_format;

    int64_t  timeline_samples;
    double   tempo_bpm;
    int32_t  time_sig_numerator;
    int32_t  time_sig_denominator;

    daux_bool is_playing;
    daux_bool is_recording;
    daux_bool is_looping;

    uint32_t reserved[8];
} daux_process_context;

typedef struct daux_process_data {
    const daux_process_context* context;

    uint32_t               input_bus_count;
    daux_audio_bus_buffer* inputs;

    uint32_t               output_bus_count;
    daux_audio_bus_buffer* outputs;

    uint32_t               num_frames;

    void*    midi_in;
    void*    midi_out;

    uint32_t reserved[4];
} daux_process_data;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_AUDIO_PROCESS_CONTEXT_H */

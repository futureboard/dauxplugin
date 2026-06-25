/*
 * DAUx/Audio/SampleFormat.h
 */
#ifndef DAUX_AUDIO_SAMPLE_FORMAT_H
#define DAUX_AUDIO_SAMPLE_FORMAT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef int32_t daux_sample_format;
enum {
    DAUX_SAMPLE_F32 = 0,
    DAUX_SAMPLE_F64 = 1
};

typedef enum DAUxSampleFormat {
    DAUX_SAMPLE_FORMAT_UNKNOWN = 0,
    DAUX_SAMPLE_FORMAT_F32     = 1,
    DAUX_SAMPLE_FORMAT_F64     = 2,
    DAUX_SAMPLE_FORMAT_I16     = 3,
    DAUX_SAMPLE_FORMAT_I24     = 4,
    DAUX_SAMPLE_FORMAT_I32     = 5
} DAUxSampleFormat;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_AUDIO_SAMPLE_FORMAT_H */

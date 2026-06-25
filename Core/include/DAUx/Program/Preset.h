#ifndef DAUX_PROGRAM_PRESET_H
#define DAUX_PROGRAM_PRESET_H

#include <stdint.h>

#include <DAUx/State/StateBlob.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxPreset {
    const char*    name;
    DAUxStateBlob  state;
    uint32_t       program_index;
} DAUxPreset;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PROGRAM_PRESET_H */

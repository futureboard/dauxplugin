#ifndef DAUX_PROGRAM_PROGRAM_H
#define DAUX_PROGRAM_PROGRAM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxProgram {
    uint32_t    index;
    const char* name;
    uint32_t    flags;
} DAUxProgram;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PROGRAM_PROGRAM_H */

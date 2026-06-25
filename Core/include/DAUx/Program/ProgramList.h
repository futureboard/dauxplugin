#ifndef DAUX_PROGRAM_PROGRAM_LIST_H
#define DAUX_PROGRAM_PROGRAM_LIST_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxProgramListInfo {
    uint32_t    list_index;
    const char* name;
    uint32_t    program_count;
} DAUxProgramListInfo;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PROGRAM_PROGRAM_LIST_H */

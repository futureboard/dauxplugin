/*
 * DAUx/State/StateBlob.h
 */
#ifndef DAUX_STATE_STATE_BLOB_H
#define DAUX_STATE_STATE_BLOB_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxStateBlob {
    const uint8_t* data;
    uint64_t       size;
} DAUxStateBlob;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_STATE_STATE_BLOB_H */

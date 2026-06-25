/*
 * DAUx/Abi/Version.h - ABI version constants and version struct.
 */
#ifndef DAUX_ABI_VERSION_H
#define DAUX_ABI_VERSION_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define DAUX_ABI_VERSION_MAJOR 1
#define DAUX_ABI_VERSION_MINOR 0
#define DAUX_ABI_VERSION_PATCH 0

#define DAUX_MAKE_VERSION(maj, min, pat) \
    ((uint32_t)(((maj) & 0xFFu) << 24) | (((min) & 0xFFu) << 16) | ((pat) & 0xFFFFu))

#define DAUX_ABI_VERSION \
    DAUX_MAKE_VERSION(DAUX_ABI_VERSION_MAJOR, DAUX_ABI_VERSION_MINOR, DAUX_ABI_VERSION_PATCH)

#define DAUX_VERSION_MAJOR(v) (((v) >> 24) & 0xFFu)
#define DAUX_VERSION_MINOR(v) (((v) >> 16) & 0xFFu)
#define DAUX_VERSION_PATCH(v) ((v) & 0xFFFFu)

typedef struct DAUxVersion {
    uint16_t major;
    uint16_t minor;
    uint16_t patch;
    uint16_t build;
} DAUxVersion;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_ABI_VERSION_H */

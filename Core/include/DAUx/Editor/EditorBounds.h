/*
 * DAUx/Editor/EditorBounds.h
 */
#ifndef DAUX_EDITOR_EDITOR_BOUNDS_H
#define DAUX_EDITOR_EDITOR_BOUNDS_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxEditorSize {
    int32_t width;
    int32_t height;
} DAUxEditorSize;

typedef struct DAUxEditorBounds {
    int32_t x;
    int32_t y;
    int32_t width;
    int32_t height;
} DAUxEditorBounds;

/* Legacy aliases. */
typedef struct daux_size {
    uint32_t width;
    uint32_t height;
} daux_size;

typedef struct daux_rect {
    int32_t  x;
    int32_t  y;
    uint32_t width;
    uint32_t height;
} daux_rect;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_EDITOR_EDITOR_BOUNDS_H */

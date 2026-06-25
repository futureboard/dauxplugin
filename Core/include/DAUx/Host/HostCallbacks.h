/*
 * DAUx/Host/HostCallbacks.h
 */
#ifndef DAUX_HOST_HOST_CALLBACKS_H
#define DAUX_HOST_HOST_CALLBACKS_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Types.h>
#include <DAUx/Abi/Version.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef void (DAUX_CALL *DAUxHostLogFn)(
    daux_host_context ctx, int32_t level, const char* utf8_msg);
typedef void (DAUX_CALL *DAUxHostParameterChangedFn)(
    daux_host_context ctx, daux_param_id id, double normalized_value);
typedef void (DAUX_CALL *DAUxHostStateChangedFn)(daux_host_context ctx);
typedef void (DAUX_CALL *DAUxHostRequestEditorResizeFn)(
    daux_host_context ctx, uint32_t width, uint32_t height);
typedef void* (DAUX_CALL *DAUxHostAllocateFn)(daux_host_context ctx, uint32_t size);
typedef void  (DAUX_CALL *DAUxHostFreeFn)(daux_host_context ctx, void* ptr);

typedef struct daux_host_callbacks {
    uint32_t struct_size;
    uint32_t abi_version;

    daux_host_context context;

    void (DAUX_CALL *param_changed)(daux_host_context ctx,
                                    daux_param_id id,
                                    double normalized_value);
    void (DAUX_CALL *param_gesture_begin)(daux_host_context ctx, daux_param_id id);
    void (DAUX_CALL *param_gesture_end)(daux_host_context ctx, daux_param_id id);
    void (DAUX_CALL *resize_editor)(daux_host_context ctx,
                                    uint32_t width, uint32_t height);
    void (DAUX_CALL *log)(daux_host_context ctx, int32_t level, const char* utf8_msg);

    void* reserved[8];
} daux_host_callbacks;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_HOST_HOST_CALLBACKS_H */

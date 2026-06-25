/*
 * daux_host.h - Host callback interface passed to a plugin instance.
 *
 * Part of "DAUx Plugin Core" / "DAUx Plugin ABI".
 *
 * The host hands the plugin a const pointer to a daux_host_callbacks vtable at
 * creation time (see daux_plugin.h: create()). The plugin may call back into
 * the host through it (e.g. to notify the host that a parameter changed from
 * the editor, or to request the editor be resized).
 *
 * All callbacks are C ABI, cdecl, and take the opaque host context as their
 * first argument. The plugin must not retain the pointer beyond its own
 * lifetime. The host owns and outlives all of these.
 */
#ifndef DAUX_HOST_H
#define DAUX_HOST_H

#include "daux_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct daux_host_callbacks {
    uint32_t struct_size;   /* sizeof(daux_host_callbacks) for fwd-compat   */
    uint32_t abi_version;   /* DAUX_ABI_VERSION the host was built against   */

    daux_host_context context; /* opaque, passed back to each callback      */

    /* Parameter changed inside the plugin (e.g. user moved a knob in the
     * editor). value is in normalized [0,1]. The host records automation. */
    void (DAUX_CALL *param_changed)(daux_host_context ctx,
                                    daux_param_id id,
                                    double normalized_value);

    /* Begin/end an automation gesture (mouse down / mouse up on a control). */
    void (DAUX_CALL *param_gesture_begin)(daux_host_context ctx, daux_param_id id);
    void (DAUX_CALL *param_gesture_end)(daux_host_context ctx, daux_param_id id);

    /* The plugin's editor wants to be resized to width x height (px). */
    void (DAUX_CALL *resize_editor)(daux_host_context ctx,
                                    uint32_t width, uint32_t height);

    /* Structured logging back to the host. level: 0=trace..4=error. */
    void (DAUX_CALL *log)(daux_host_context ctx, int32_t level, const char* utf8_msg);

    void* reserved[8];
} daux_host_callbacks;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_HOST_H */

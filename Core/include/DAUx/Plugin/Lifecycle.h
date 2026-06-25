/*
 * DAUx/Plugin/Lifecycle.h - Opaque handles and lifecycle callback typedefs.
 */
#ifndef DAUX_PLUGIN_LIFECYCLE_H
#define DAUX_PLUGIN_LIFECYCLE_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/Abi/Types.h>
#include <DAUx/Audio/ProcessContext.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxHost            DAUxHost;
typedef struct DAUxPluginInstance  DAUxPluginInstance;
typedef struct DAUxEditorInstance  DAUxEditorInstance;

typedef daux_result (DAUX_CALL *DAUxPluginCreateFn)(
    DAUxHost* host, DAUxPluginInstance** out_instance);
typedef daux_result (DAUX_CALL *DAUxPluginDestroyFn)(DAUxPluginInstance* instance);
typedef daux_result (DAUX_CALL *DAUxPluginPrepareFn)(
    DAUxPluginInstance* instance, double sample_rate, uint32_t max_block_size);
typedef daux_result (DAUX_CALL *DAUxPluginActivateFn)(DAUxPluginInstance* instance);
typedef daux_result (DAUX_CALL *DAUxPluginDeactivateFn)(DAUxPluginInstance* instance);
typedef daux_result (DAUX_CALL *DAUxPluginResetFn)(DAUxPluginInstance* instance);
typedef daux_result (DAUX_CALL *DAUxPluginProcessFn)(
    DAUxPluginInstance* instance, const daux_process_data* data);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PLUGIN_LIFECYCLE_H */

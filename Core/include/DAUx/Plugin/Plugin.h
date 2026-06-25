/*
 * DAUx/Plugin/Plugin.h - Plugin factory, vtable and per-instance API (stable C ABI).
 */
#ifndef DAUX_PLUGIN_PLUGIN_H
#define DAUX_PLUGIN_PLUGIN_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/Abi/Types.h>
#include <DAUx/Abi/Version.h>
#include <DAUx/Audio/ProcessContext.h>
#include <DAUx/Editor/Editor.h>
#include <DAUx/Host/HostCallbacks.h>
#include <DAUx/Parameter/ParameterInfo.h>
#include <DAUx/Parameter/Parameter.h>
#include <DAUx/State/State.h>
#include <DAUx/Plugin/Descriptor.h>
#include <DAUx/Plugin/Lifecycle.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct daux_plugin_vtable {
    uint32_t struct_size;

    daux_result (DAUX_CALL *prepare)(daux_plugin_handle self,
                                     double sample_rate, uint32_t max_block_size);
    daux_result (DAUX_CALL *activate)(daux_plugin_handle self);
    daux_result (DAUX_CALL *deactivate)(daux_plugin_handle self);
    daux_result (DAUX_CALL *reset)(daux_plugin_handle self);
    daux_result (DAUX_CALL *process)(daux_plugin_handle self,
                                     const daux_process_data* data);

    uint32_t    (DAUX_CALL *get_parameter_count)(daux_plugin_handle self);
    daux_result (DAUX_CALL *get_parameter_info)(daux_plugin_handle self,
                                                uint32_t index,
                                                daux_param_info* out_info);
    daux_result (DAUX_CALL *get_parameter_value)(daux_plugin_handle self,
                                                 daux_param_id id,
                                                 double* out_value);
    daux_result (DAUX_CALL *set_parameter_value)(daux_plugin_handle self,
                                                 daux_param_id id,
                                                 double value);

    double (DAUX_CALL *normalized_to_plain)(daux_plugin_handle self,
                                            daux_param_id id, double normalized);
    double (DAUX_CALL *plain_to_normalized)(daux_plugin_handle self,
                                            daux_param_id id, double plain);

    daux_result (DAUX_CALL *format_parameter)(daux_plugin_handle self,
                                              daux_param_id id, double plain,
                                              char* out_buf, uint32_t buf_size);
    daux_result (DAUX_CALL *parse_parameter)(daux_plugin_handle self,
                                             daux_param_id id, const char* text,
                                             double* out_plain);

    daux_result (DAUX_CALL *get_state)(daux_plugin_handle self,
                                     void* out_buf, uint32_t buf_size,
                                     uint32_t* out_size);
    daux_result (DAUX_CALL *set_state)(daux_plugin_handle self,
                                       const void* data, uint32_t size);

    daux_result (DAUX_CALL *create_editor)(daux_plugin_handle self,
                                           const daux_host_callbacks* host,
                                           daux_editor* out_editor);
    daux_result (DAUX_CALL *destroy_editor)(daux_plugin_handle self,
                                            daux_editor_handle editor);

    void* reserved[8];
} daux_plugin_vtable;

typedef struct daux_plugin_instance {
    daux_plugin_handle        handle;
    const daux_plugin_vtable* vtable;
} daux_plugin_instance;

typedef struct daux_plugin_factory {
    uint32_t struct_size;
    uint32_t abi_version;

    const daux_plugin_descriptor* (DAUX_CALL *get_descriptor)(void);
    daux_result (DAUX_CALL *create)(const daux_host_callbacks* host,
                                    daux_plugin_instance* out_instance);
    daux_result (DAUX_CALL *destroy)(daux_plugin_handle self);

    void* reserved[6];
} daux_plugin_factory;

/* Future vtable layout (not used by the current host loader). */
typedef struct DAUxPluginVTable {
    DAUxPluginCreateFn     create;
    DAUxPluginDestroyFn    destroy;
    DAUxPluginPrepareFn    prepare;
    DAUxPluginActivateFn   activate;
    DAUxPluginDeactivateFn deactivate;
    DAUxPluginResetFn      reset;
    DAUxPluginProcessFn    process;

    DAUxPluginGetParameterCountFn     get_parameter_count;
    DAUxPluginGetParameterInfoFn      get_parameter_info;
    DAUxPluginGetParameterValueFn     get_parameter_value;
    DAUxPluginSetParameterValueFn     set_parameter_value;

    DAUxPluginGetStateFn get_state;
    DAUxPluginSetStateFn set_state;

    DAUxEditorCreateFn           create_editor;
    DAUxEditorDestroyFn          destroy_editor;
    DAUxEditorGetPreferredSizeFn get_preferred_size;
    DAUxEditorSetParentFn        set_parent;
    DAUxEditorSetBoundsFn        set_bounds;
    DAUxEditorShowFn             show;
    DAUxEditorHideFn             hide;
    DAUxEditorTickFn             tick;
} DAUxPluginVTable;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PLUGIN_PLUGIN_H */

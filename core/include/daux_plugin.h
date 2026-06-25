/*
 * daux_plugin.h - The plugin entry point, factory and per-instance vtable.
 *
 * Part of "DAUx Plugin Core" / "DAUx Plugin ABI".
 *
 * Loading model
 * -------------
 * A .dauxplug is a dynamic library (DLL on Windows) that exports exactly one
 * symbol:
 *
 *     const daux_plugin_factory* daux_plugin_entry(uint32_t host_abi_version);
 *
 * The host:
 *   1. dlopen/LoadLibrary the file,
 *   2. resolve "daux_plugin_entry",
 *   3. call it with DAUX_ABI_VERSION,
 *   4. validate factory->abi_version major == host major,
 *   5. read factory->get_descriptor() (cheap; used by DAUxScan),
 *   6. optionally factory->create() to instantiate.
 *
 * The factory and descriptor are static/owned by the plugin module and live for
 * as long as the module is loaded. Instances are created/destroyed explicitly.
 */
#ifndef DAUX_PLUGIN_H
#define DAUX_PLUGIN_H

#include "daux_types.h"
#include "daux_host.h"
#include "daux_editor.h"

#ifdef __cplusplus
extern "C" {
#endif

/* The exported entry point symbol name (resolved via GetProcAddress/dlsym). */
#define DAUX_PLUGIN_ENTRY_SYMBOL "daux_plugin_entry"

/* ===========================================================================
 * Per-instance vtable
 * ===========================================================================
 * Every function takes the plugin instance handle as its first argument. The
 * handle is whatever create() returned. All functions are C ABI / cdecl.
 *
 * Threading contract:
 *   - process() runs on the realtime audio thread; it must not allocate or
 *     block. All other calls happen on a non-realtime thread, never
 *     concurrently with process().
 */
typedef struct daux_plugin_vtable {
    uint32_t struct_size;   /* sizeof(daux_plugin_vtable) */

    /* ---- lifecycle ------------------------------------------------------ */

    /* Called once after create(), before any processing, and again whenever
     * the sample rate or max block size changes. Allocation is allowed here. */
    daux_result (DAUX_CALL *prepare)(daux_plugin_handle self,
                                     double   sample_rate,
                                     uint32_t max_block_size);

    /* Move between active (ready to process) and inactive states. */
    daux_result (DAUX_CALL *activate)(daux_plugin_handle self);
    daux_result (DAUX_CALL *deactivate)(daux_plugin_handle self);

    /* Clear internal buffers / tails. Cheap; may be called from prepare. */
    daux_result (DAUX_CALL *reset)(daux_plugin_handle self);

    /* ---- realtime processing ------------------------------------------- */
    daux_result (DAUX_CALL *process)(daux_plugin_handle self,
                                     const daux_process_data* data);

    /* ---- parameters ----------------------------------------------------- */
    uint32_t    (DAUX_CALL *get_parameter_count)(daux_plugin_handle self);

    daux_result (DAUX_CALL *get_parameter_info)(daux_plugin_handle self,
                                                uint32_t index,
                                                daux_param_info* out_info);

    /* Values are exchanged in PLAIN units across these two calls. */
    daux_result (DAUX_CALL *get_parameter_value)(daux_plugin_handle self,
                                                 daux_param_id id,
                                                 double* out_value);
    daux_result (DAUX_CALL *set_parameter_value)(daux_plugin_handle self,
                                                 daux_param_id id,
                                                 double value);

    /* normalized [0,1] <-> plain conversions. May be NULL for linear params,
     * in which case the host assumes a linear map between min and max. */
    double (DAUX_CALL *normalized_to_plain)(daux_plugin_handle self,
                                            daux_param_id id, double normalized);
    double (DAUX_CALL *plain_to_normalized)(daux_plugin_handle self,
                                            daux_param_id id, double plain);

    /* Optional string formatting / parsing. May be NULL. out_buf is a
     * caller-provided UTF-8 buffer of buf_size bytes; on success the function
     * writes a NUL-terminated string. */
    daux_result (DAUX_CALL *format_parameter)(daux_plugin_handle self,
                                              daux_param_id id, double plain,
                                              char* out_buf, uint32_t buf_size);
    daux_result (DAUX_CALL *parse_parameter)(daux_plugin_handle self,
                                             daux_param_id id, const char* text,
                                             double* out_plain);

    /* ---- state / preset ------------------------------------------------- */
    /* get_state writes up to buf_size bytes into out_buf and sets
     * *out_size to the number of bytes the full state needs. If out_buf is
     * NULL or too small, returns DAUX_ERR_BUFFER_TOO_SMALL with *out_size set
     * (so the host can size-then-fetch). May be NULL if !DAUX_CAP_STATE. */
    daux_result (DAUX_CALL *get_state)(daux_plugin_handle self,
                                       void* out_buf, uint32_t buf_size,
                                       uint32_t* out_size);
    daux_result (DAUX_CALL *set_state)(daux_plugin_handle self,
                                       const void* data, uint32_t size);

    /* ---- editor --------------------------------------------------------- */
    /* Returns an editor instance. out_editor->handle / vtable are owned by the
     * plugin instance and valid until destroy_editor() / destroy(). May be
     * NULL if !DAUX_CAP_EDITOR. */
    daux_result (DAUX_CALL *create_editor)(daux_plugin_handle self,
                                           const daux_host_callbacks* host,
                                           daux_editor* out_editor);
    daux_result (DAUX_CALL *destroy_editor)(daux_plugin_handle self,
                                            daux_editor_handle editor);

    void* reserved[8];
} daux_plugin_vtable;

/* A created instance: opaque handle + its vtable. */
typedef struct daux_plugin_instance {
    daux_plugin_handle        handle;
    const daux_plugin_vtable* vtable;
} daux_plugin_instance;

/* ===========================================================================
 * Factory (module level)
 * =========================================================================== */
typedef struct daux_plugin_factory {
    uint32_t struct_size;   /* sizeof(daux_plugin_factory)               */
    uint32_t abi_version;   /* DAUX_ABI_VERSION the plugin was built with */

    /* Cheap, instantiation-free descriptor read (used by DAUxScan). */
    const daux_plugin_descriptor* (DAUX_CALL *get_descriptor)(void);

    /* Create / destroy an instance. host may be NULL for headless scanning,
     * but a real run should pass a valid callback table. */
    daux_result (DAUX_CALL *create)(const daux_host_callbacks* host,
                                    daux_plugin_instance* out_instance);
    daux_result (DAUX_CALL *destroy)(daux_plugin_handle self);

    void* reserved[6];
} daux_plugin_factory;

/* The single exported entry point. Implemented by every plugin. The host calls
 * it with its own ABI version so the plugin can refuse a mismatched host. */
typedef const daux_plugin_factory* (DAUX_CALL *daux_plugin_entry_fn)(uint32_t host_abi_version);

DAUX_API const daux_plugin_factory* DAUX_CALL daux_plugin_entry(uint32_t host_abi_version);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PLUGIN_H */

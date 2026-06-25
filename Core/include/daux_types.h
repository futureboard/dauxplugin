/*
 * daux_types.h - Core POD types, enums and result codes for the DAUx Plugin ABI.
 *
 * Part of "DAUx Plugin Core" / "DAUx Plugin ABI".
 *
 * Design rules enforced here:
 *   - Only fixed-size integer types, plain enums (as int32) and POD structs.
 *   - No pointers to STL/owning containers. Strings are UTF-8, NUL terminated,
 *     fixed-size inline buffers OR caller-provided buffers (see daux_plugin.h).
 *   - All structs are explicitly laid out; do not reorder fields without
 *     bumping DAUX_ABI_VERSION.
 */
#ifndef DAUX_TYPES_H
#define DAUX_TYPES_H

#include <stdint.h>
#include <stddef.h>

#include "daux_export.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ===========================================================================
 * ABI version
 * ===========================================================================
 * Bumped whenever the binary layout of any public struct or the meaning of any
 * function changes. The host refuses plugins whose factory reports a different
 * major version. */
#define DAUX_ABI_VERSION_MAJOR 1
#define DAUX_ABI_VERSION_MINOR 0
#define DAUX_ABI_VERSION_PATCH 0

/* Packed into a single uint32 as 0xMMmmpppp (8/8/16 bits). */
#define DAUX_MAKE_VERSION(maj, min, pat) \
    ((uint32_t)(((maj) & 0xFFu) << 24) | (((min) & 0xFFu) << 16) | ((pat) & 0xFFFFu))

#define DAUX_ABI_VERSION \
    DAUX_MAKE_VERSION(DAUX_ABI_VERSION_MAJOR, DAUX_ABI_VERSION_MINOR, DAUX_ABI_VERSION_PATCH)

#define DAUX_VERSION_MAJOR(v) (((v) >> 24) & 0xFFu)
#define DAUX_VERSION_MINOR(v) (((v) >> 16) & 0xFFu)
#define DAUX_VERSION_PATCH(v) ((v) & 0xFFFFu)

/* ===========================================================================
 * Fixed-string sizes (inline UTF-8 buffers in descriptor structs)
 * =========================================================================== */
#define DAUX_ID_SIZE        64   /* reverse-dns-ish plugin id           */
#define DAUX_NAME_SIZE      128  /* display name                        */
#define DAUX_SHORT_SIZE     32   /* short name / unit                   */
#define DAUX_VENDOR_SIZE    128  /* vendor / manufacturer               */

/* ===========================================================================
 * Result codes
 * ===========================================================================
 * 0 == success. Negative == error. Keep stable; only append. */
typedef int32_t daux_result;

enum {
    DAUX_OK                  = 0,
    DAUX_ERR_UNKNOWN         = -1,
    DAUX_ERR_INVALID_ARG     = -2,
    DAUX_ERR_NOT_SUPPORTED   = -3,
    DAUX_ERR_NOT_INITIALIZED = -4,
    DAUX_ERR_OUT_OF_MEMORY   = -5,
    DAUX_ERR_INVALID_STATE   = -6,
    DAUX_ERR_BUFFER_TOO_SMALL= -7,  /* caller buffer too small; required size returned */
    DAUX_ERR_ABI_MISMATCH    = -8,
    DAUX_ERR_NOT_FOUND       = -9,
    DAUX_ERR_IO              = -10
};

/* ===========================================================================
 * Boolean (avoid C++ bool ABI ambiguity)
 * =========================================================================== */
typedef int32_t daux_bool;
#define DAUX_TRUE  1
#define DAUX_FALSE 0

/* ===========================================================================
 * Opaque handles
 * =========================================================================== */
typedef void* daux_plugin_handle;   /* one plugin instance              */
typedef void* daux_editor_handle;   /* one editor instance              */
typedef void* daux_host_context;    /* opaque host-owned context        */

/* Native window handle for editor parenting. On Windows this is an HWND. */
typedef void* daux_native_window;

/* Parameter identifier. Stable per-plugin; chosen by the plugin author. */
typedef uint32_t daux_param_id;

/* ===========================================================================
 * Plugin category / type
 * =========================================================================== */
typedef int32_t daux_plugin_category;
enum {
    DAUX_CATEGORY_UNKNOWN    = 0,
    DAUX_CATEGORY_EFFECT     = 1,   /* audio in -> audio out            */
    DAUX_CATEGORY_INSTRUMENT = 2,   /* midi in  -> audio out            */
    DAUX_CATEGORY_MIDI_FX    = 3,   /* midi in  -> midi out             */
    DAUX_CATEGORY_ANALYZER   = 4    /* audio in -> (no audio out)       */
};

/* ===========================================================================
 * Sample format (float32 baseline; float64 reserved for the future)
 * =========================================================================== */
typedef int32_t daux_sample_format;
enum {
    DAUX_SAMPLE_F32 = 0,
    DAUX_SAMPLE_F64 = 1   /* TODO: not yet honored by the reference core */
};

/* ===========================================================================
 * Process context
 * ===========================================================================
 * Snapshot of transport/timeline state for a single process() call. POD. */
typedef struct daux_process_context {
    double   sample_rate;        /* e.g. 48000.0                          */
    uint32_t block_size;         /* number of frames in this block        */
    uint32_t sample_format;      /* daux_sample_format                    */

    int64_t  timeline_samples;   /* sample position on the project timeline */
    double   tempo_bpm;          /* current tempo                         */
    int32_t  time_sig_numerator; /* e.g. 4                                */
    int32_t  time_sig_denominator;/* e.g. 4                               */

    daux_bool is_playing;
    daux_bool is_recording;
    daux_bool is_looping;

    uint32_t reserved[8];        /* keep ABI room; must be zero-filled    */
} daux_process_context;

/* ===========================================================================
 * Audio buffers
 * ===========================================================================
 * Non-interleaved (planar) channel pointers per bus. The host owns the memory
 * and guarantees it is valid only for the duration of process().
 *
 * For float32, channels[c] points to `num_frames` contiguous floats.
 * channels64 is the float64 view; only one is valid per call, selected by
 * daux_process_context.sample_format. */
typedef struct daux_audio_bus_buffer {
    uint32_t channel_count;
    float**  channels;     /* valid when sample_format == DAUX_SAMPLE_F32 */
    double** channels64;   /* valid when sample_format == DAUX_SAMPLE_F64 */
} daux_audio_bus_buffer;

typedef struct daux_process_data {
    const daux_process_context* context;

    uint32_t                input_bus_count;
    daux_audio_bus_buffer*  inputs;        /* length == input_bus_count   */

    uint32_t                output_bus_count;
    daux_audio_bus_buffer*  outputs;       /* length == output_bus_count  */

    uint32_t                num_frames;    /* frames to process this call */

    /* MIDI is reserved for a future revision; pointers are NULL for now.
     * Kept in the struct so adding it later does not shift earlier fields. */
    void*    midi_in;     /* TODO: daux_midi_event_list*                  */
    void*    midi_out;    /* TODO: daux_midi_event_list*                  */

    uint32_t reserved[4];
} daux_process_data;

/* ===========================================================================
 * Parameter info
 * ===========================================================================
 * Returned by daux_plugin_vtable.get_parameter_info(). POD with inline names so
 * the host need not manage plugin-owned string lifetimes. */
typedef int32_t daux_param_flags;
enum {
    DAUX_PARAM_NONE        = 0,
    DAUX_PARAM_AUTOMATABLE = 1 << 0,
    DAUX_PARAM_STEPPED     = 1 << 1,  /* discrete steps (step_count valid) */
    DAUX_PARAM_READ_ONLY   = 1 << 2,
    DAUX_PARAM_IS_BYPASS   = 1 << 3   /* the canonical bypass parameter    */
};

typedef struct daux_param_info {
    daux_param_id id;
    char          name[DAUX_NAME_SIZE];
    char          short_name[DAUX_SHORT_SIZE];
    char          unit[DAUX_SHORT_SIZE];

    double        default_value;   /* in plain units                      */
    double        min_value;       /* plain                               */
    double        max_value;       /* plain                               */
    int32_t       step_count;      /* 0 == continuous                     */

    daux_param_flags flags;
    uint32_t      reserved[4];
} daux_param_info;

/* ===========================================================================
 * Plugin descriptor
 * ===========================================================================
 * One per plugin (not per instance). Exposed by the factory; the host may read
 * it without instantiating the plugin (used by DAUxScan). */
typedef int32_t daux_plugin_caps;
enum {
    DAUX_CAP_NONE         = 0,
    DAUX_CAP_EDITOR       = 1 << 0,
    DAUX_CAP_MIDI_INPUT   = 1 << 1,
    DAUX_CAP_MIDI_OUTPUT  = 1 << 2,
    DAUX_CAP_STATE        = 1 << 3,  /* supports get_state/set_state       */
    DAUX_CAP_PRESETS      = 1 << 4
};

typedef struct daux_plugin_descriptor {
    char     id[DAUX_ID_SIZE];           /* e.g. "com.daux.examples.gain"  */
    char     name[DAUX_NAME_SIZE];
    char     vendor[DAUX_VENDOR_SIZE];

    uint32_t version;                    /* DAUX_MAKE_VERSION packed        */
    daux_plugin_category category;
    daux_plugin_caps     capabilities;   /* OR of DAUX_CAP_*                */

    uint32_t audio_input_buses;
    uint32_t audio_output_buses;

    /* Default channel count assumed per bus when the host does not negotiate.
     * 0 -> use 2 (stereo). A richer bus-layout negotiation API is a TODO. */
    uint32_t default_channels_per_bus;

    uint32_t reserved[8];
} daux_plugin_descriptor;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_TYPES_H */

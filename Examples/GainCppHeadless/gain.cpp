/*
 * gain.cpp - Reference headless C++ DAUx plugin: "DAUx Gain (C++)".
 *
 * This is the canonical example that exercises the full plugin ABI:
 *   - exports daux_plugin_entry,
 *   - one automatable "Gain" parameter (in dB),
 *   - planar float32 process() that applies linear gain input -> output,
 *   - get_state/set_state round-tripping the parameter,
 *   - no editor (DAUX_CAP_EDITOR not set).
 *
 * It depends ONLY on the public C ABI (daux::abi), never on DAUx/Core.hpp.
 */
#include <DAUx/DAUx.h>

#include <cmath>
#include <cstdio>
#include <cstring>
#include <cstdint>
#include <new>

namespace {

constexpr daux_param_id PARAM_GAIN = 1;

constexpr double GAIN_MIN_DB = -60.0;
constexpr double GAIN_MAX_DB =  12.0;
constexpr double GAIN_DEF_DB =   0.0;

inline double db_to_linear(double db) { return std::pow(10.0, db / 20.0); }

/* ---- instance ----------------------------------------------------------- */
struct GainInstance {
    const daux_host_callbacks* host = nullptr;
    double sample_rate = 0.0;
    uint32_t max_block = 0;
    double gain_db = GAIN_DEF_DB;   // the single parameter, plain units (dB)
    bool active = false;
};

inline GainInstance* self_of(daux_plugin_handle h) {
    return static_cast<GainInstance*>(h);
}

/* ---- lifecycle ---------------------------------------------------------- */
daux_result DAUX_CALL vt_prepare(daux_plugin_handle h, double sr, uint32_t mb) {
    auto* s = self_of(h);
    s->sample_rate = sr;
    s->max_block = mb;
    return DAUX_OK;
}
daux_result DAUX_CALL vt_activate(daux_plugin_handle h)   { self_of(h)->active = true;  return DAUX_OK; }
daux_result DAUX_CALL vt_deactivate(daux_plugin_handle h) { self_of(h)->active = false; return DAUX_OK; }
daux_result DAUX_CALL vt_reset(daux_plugin_handle)        { return DAUX_OK; }

/* ---- process ------------------------------------------------------------ */
daux_result DAUX_CALL vt_process(daux_plugin_handle h, const daux_process_data* d) {
    if (!d) return DAUX_ERR_INVALID_ARG;
    auto* s = self_of(h);
    const float g = (float)db_to_linear(s->gain_db);
    const uint32_t frames = d->num_frames;

    /* Apply gain per matching in/out bus & channel. Where there is no matching
     * input channel, emit silence (so the output is always well-defined). */
    for (uint32_t bus = 0; bus < d->output_bus_count; ++bus) {
        daux_audio_bus_buffer& out = d->outputs[bus];
        const daux_audio_bus_buffer* in =
            (bus < d->input_bus_count) ? &d->inputs[bus] : nullptr;

        for (uint32_t ch = 0; ch < out.channel_count; ++ch) {
            float* dst = out.channels ? out.channels[ch] : nullptr;
            if (!dst) continue;
            const float* src =
                (in && in->channels && ch < in->channel_count) ? in->channels[ch] : nullptr;
            if (src) {
                for (uint32_t i = 0; i < frames; ++i) dst[i] = src[i] * g;
            } else {
                std::memset(dst, 0, sizeof(float) * frames);
            }
        }
    }
    return DAUX_OK;
}

/* ---- parameters --------------------------------------------------------- */
uint32_t DAUX_CALL vt_param_count(daux_plugin_handle) { return 1; }

daux_result DAUX_CALL vt_param_info(daux_plugin_handle, uint32_t index, daux_param_info* out) {
    if (!out) return DAUX_ERR_INVALID_ARG;
    if (index != 0) return DAUX_ERR_NOT_FOUND;
    std::memset(out, 0, sizeof(*out));
    out->id = PARAM_GAIN;
    std::strncpy(out->name, "Gain", DAUX_NAME_SIZE - 1);
    std::strncpy(out->short_name, "Gain", DAUX_SHORT_SIZE - 1);
    std::strncpy(out->unit, "dB", DAUX_SHORT_SIZE - 1);
    out->default_value = GAIN_DEF_DB;
    out->min_value = GAIN_MIN_DB;
    out->max_value = GAIN_MAX_DB;
    out->step_count = 0;
    out->flags = DAUX_PARAM_AUTOMATABLE;
    return DAUX_OK;
}

daux_result DAUX_CALL vt_param_get(daux_plugin_handle h, daux_param_id id, double* out) {
    if (!out) return DAUX_ERR_INVALID_ARG;
    if (id != PARAM_GAIN) return DAUX_ERR_NOT_FOUND;
    *out = self_of(h)->gain_db;
    return DAUX_OK;
}

daux_result DAUX_CALL vt_param_set(daux_plugin_handle h, daux_param_id id, double v) {
    if (id != PARAM_GAIN) return DAUX_ERR_NOT_FOUND;
    if (v < GAIN_MIN_DB) v = GAIN_MIN_DB;
    if (v > GAIN_MAX_DB) v = GAIN_MAX_DB;
    self_of(h)->gain_db = v;
    return DAUX_OK;
}

double DAUX_CALL vt_norm_to_plain(daux_plugin_handle, daux_param_id, double n) {
    return GAIN_MIN_DB + n * (GAIN_MAX_DB - GAIN_MIN_DB);
}
double DAUX_CALL vt_plain_to_norm(daux_plugin_handle, daux_param_id, double p) {
    return (p - GAIN_MIN_DB) / (GAIN_MAX_DB - GAIN_MIN_DB);
}

daux_result DAUX_CALL vt_format(daux_plugin_handle, daux_param_id id, double plain,
                                char* buf, uint32_t size) {
    if (!buf || size == 0 || id != PARAM_GAIN) return DAUX_ERR_INVALID_ARG;
    std::snprintf(buf, size, "%.1f dB", plain);
    return DAUX_OK;
}
daux_result DAUX_CALL vt_parse(daux_plugin_handle, daux_param_id id, const char* text,
                               double* out) {
    if (!text || !out || id != PARAM_GAIN) return DAUX_ERR_INVALID_ARG;
    return (std::sscanf(text, "%lf", out) == 1) ? DAUX_OK : DAUX_ERR_INVALID_ARG;
}

/* ---- state -------------------------------------------------------------- */
/* Trivial format: a 4-byte magic + the gain (double, little-endian host). */
static const uint32_t STATE_MAGIC = 0x47584144; // "DAXG"

daux_result DAUX_CALL vt_get_state(daux_plugin_handle h, void* buf, uint32_t size, uint32_t* out_size) {
    const uint32_t need = sizeof(uint32_t) + sizeof(double);
    if (out_size) *out_size = need;
    if (!buf || size < need) return DAUX_ERR_BUFFER_TOO_SMALL;
    auto* s = self_of(h);
    std::memcpy(buf, &STATE_MAGIC, sizeof(uint32_t));
    std::memcpy(static_cast<uint8_t*>(buf) + sizeof(uint32_t), &s->gain_db, sizeof(double));
    return DAUX_OK;
}

daux_result DAUX_CALL vt_set_state(daux_plugin_handle h, const void* data, uint32_t size) {
    const uint32_t need = sizeof(uint32_t) + sizeof(double);
    if (!data || size < need) return DAUX_ERR_INVALID_ARG;
    uint32_t magic = 0;
    std::memcpy(&magic, data, sizeof(uint32_t));
    if (magic != STATE_MAGIC) return DAUX_ERR_INVALID_STATE;
    double g = 0.0;
    std::memcpy(&g, static_cast<const uint8_t*>(data) + sizeof(uint32_t), sizeof(double));
    return vt_param_set(h, PARAM_GAIN, g);
}

/* ---- editor (unsupported) ----------------------------------------------- */
daux_result DAUX_CALL vt_create_editor(daux_plugin_handle, const daux_host_callbacks*, daux_editor*) {
    return DAUX_ERR_NOT_SUPPORTED;
}
daux_result DAUX_CALL vt_destroy_editor(daux_plugin_handle, daux_editor_handle) {
    return DAUX_ERR_NOT_SUPPORTED;
}

/* ---- vtable / factory --------------------------------------------------- */
const daux_plugin_vtable g_vtable = [] {
    daux_plugin_vtable v{};
    v.struct_size = sizeof(daux_plugin_vtable);
    v.prepare = vt_prepare;
    v.activate = vt_activate;
    v.deactivate = vt_deactivate;
    v.reset = vt_reset;
    v.process = vt_process;
    v.get_parameter_count = vt_param_count;
    v.get_parameter_info = vt_param_info;
    v.get_parameter_value = vt_param_get;
    v.set_parameter_value = vt_param_set;
    v.normalized_to_plain = vt_norm_to_plain;
    v.plain_to_normalized = vt_plain_to_norm;
    v.format_parameter = vt_format;
    v.parse_parameter = vt_parse;
    v.get_state = vt_get_state;
    v.set_state = vt_set_state;
    v.create_editor = vt_create_editor;
    v.destroy_editor = vt_destroy_editor;
    return v;
}();

const daux_plugin_descriptor g_descriptor = [] {
    daux_plugin_descriptor d{};
    std::strncpy(d.id, "com.daux.examples.gain.cpp", DAUX_ID_SIZE - 1);
    std::strncpy(d.name, "DAUx Gain (C++)", DAUX_NAME_SIZE - 1);
    std::strncpy(d.vendor, "DAUx", DAUX_VENDOR_SIZE - 1);
    d.version = DAUX_MAKE_VERSION(0, 1, 0);
    d.category = DAUX_CATEGORY_EFFECT;
    d.capabilities = DAUX_CAP_STATE; // no editor, no midi
    d.audio_input_buses = 1;
    d.audio_output_buses = 1;
    d.default_channels_per_bus = 2;
    return d;
}();

const daux_plugin_descriptor* DAUX_CALL fac_get_descriptor(void) { return &g_descriptor; }

daux_result DAUX_CALL fac_create(const daux_host_callbacks* host, daux_plugin_instance* out) {
    if (!out) return DAUX_ERR_INVALID_ARG;
    auto* s = new (std::nothrow) GainInstance();
    if (!s) return DAUX_ERR_OUT_OF_MEMORY;
    s->host = host;
    out->handle = s;
    out->vtable = &g_vtable;
    return DAUX_OK;
}

daux_result DAUX_CALL fac_destroy(daux_plugin_handle h) {
    delete self_of(h);
    return DAUX_OK;
}

const daux_plugin_factory g_factory = [] {
    daux_plugin_factory f{};
    f.struct_size = sizeof(daux_plugin_factory);
    f.abi_version = DAUX_ABI_VERSION;
    f.get_descriptor = fac_get_descriptor;
    f.create = fac_create;
    f.destroy = fac_destroy;
    return f;
}();

} // namespace

DAUX_API const daux_plugin_factory* DAUX_CALL daux_plugin_entry(uint32_t host_abi_version) {
    /* Refuse a host whose major ABI differs from ours. */
    if (DAUX_VERSION_MAJOR(host_abi_version) != DAUX_ABI_VERSION_MAJOR)
        return nullptr;
    return &g_factory;
}

/* Optional modern factory export (additive; legacy entry remains authoritative). */
static uint32_t DAUX_CALL mf_class_count(void) { return 1; }

static daux_result DAUX_CALL mf_class_info(uint32_t index, DAUxPluginClassInfo* out) {
    if (!out || index != 0) return DAUX_ERR_INVALID_ARG;
    std::memset(out, 0, sizeof(*out));
    out->class_id = "com.daux.examples.gain.cpp";
    out->name = "DAUx Gain (C++)";
    out->vendor = "DAUx";
    out->version = {1, 0, 0, 0};
    out->category = DAUX_PLUGIN_CATEGORY_EFFECT;
    out->capabilities = DAUX_CAP_STATE;
    out->gui_framework = DAUX_GUI_FRAMEWORK_NATIVE;
    out->default_editor_mode = DAUX_EDITOR_MODE_EMBEDDED;
    out->audio_input_bus_count = 1;
    out->audio_output_bus_count = 1;
    return DAUX_OK;
}

static const daux_plugin_descriptor* DAUX_CALL mf_legacy_descriptor(void) {
    return fac_get_descriptor();
}

static const DAUxPluginFactory g_modern_factory = [] {
    DAUxPluginFactory f{};
    f.struct_size = sizeof(DAUxPluginFactory);
    f.abi_version = DAUX_ABI_VERSION;
    f.get_class_count = mf_class_count;
    f.get_class_info = mf_class_info;
    f.create_component_by_class_id = nullptr;
    f.get_legacy_descriptor = mf_legacy_descriptor;
    return f;
}();

DAUX_API const DAUxPluginFactory* daux_get_plugin_factory(void) {
    return &g_modern_factory;
}

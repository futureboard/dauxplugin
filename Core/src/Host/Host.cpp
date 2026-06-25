/*
 * Host.cpp - Default host callback table for headless processing/scanning.
 */
#include <DAUx/Core.hpp>

#include <cstdio>

namespace daux {
namespace {

void DAUX_CALL cb_param_changed(daux_host_context, daux_param_id, double) {}
void DAUX_CALL cb_gesture_begin(daux_host_context, daux_param_id) {}
void DAUX_CALL cb_gesture_end(daux_host_context, daux_param_id) {}
void DAUX_CALL cb_resize_editor(daux_host_context, uint32_t, uint32_t) {}

void DAUX_CALL cb_log(daux_host_context, int32_t level, const char* msg) {
    static const char* names[] = {"TRACE", "DEBUG", "INFO", "WARN", "ERROR"};
    const char* lvl = (level >= 0 && level <= 4) ? names[level] : "LOG";
    std::fprintf(stderr, "[plugin %s] %s\n", lvl, msg ? msg : "");
}

daux_host_callbacks make_default() {
    daux_host_callbacks cb{};
    cb.struct_size = (uint32_t)sizeof(daux_host_callbacks);
    cb.abi_version = DAUX_ABI_VERSION;
    cb.context = nullptr;
    cb.param_changed = cb_param_changed;
    cb.param_gesture_begin = cb_gesture_begin;
    cb.param_gesture_end = cb_gesture_end;
    cb.resize_editor = cb_resize_editor;
    cb.log = cb_log;
    return cb;
}

} // namespace

const daux_host_callbacks* default_host_callbacks() noexcept {
    static daux_host_callbacks cb = make_default();
    return &cb;
}

} // namespace daux

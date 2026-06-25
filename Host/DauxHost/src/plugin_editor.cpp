/*
 * plugin_editor.cpp - Editor hosting helpers (skeleton).
 *
 * This is the home for the window-management code that hosts a plugin editor
 * inside (or out-of-process from) the DAW. The cross-language goal: a Rust GPUI
 * editor or a C# Avalonia editor created by a wrapper attaches into a host
 * native window through the daux_editor_vtable, exactly like a native C++ one.
 *
 * Only a tiny helper is implemented now; full window/message-loop hosting is a
 * TODO tracked against DAUxHost --mode=editor.
 */
#include "host.hpp"
#include <DAUx/Editor/Editor.h>

#include <cstdio>

namespace dauxhost {

/* Create the plugin's editor and report its preferred size. Returns false if
 * the plugin has no editor or creation fails. Does NOT attach to a window yet
 * (no window system wired up in this milestone). */
bool probe_editor(daux::PluginInstance& inst, const daux_host_callbacks* host) {
    const daux_plugin_vtable* v = inst.vtable();
    if (!v || !v->create_editor) return false;

    daux_editor ed{};
    daux_result r = v->create_editor(inst.handle(), host, &ed);
    if (r != DAUX_OK || !ed.handle || !ed.vtable) {
        std::fprintf(stderr, "create_editor failed: %s\n", daux::result_to_string(r));
        return false;
    }

    daux_size size{0, 0};
    if (ed.vtable->get_preferred_size)
        ed.vtable->get_preferred_size(ed.handle, &size);
    std::printf("editor created; preferred size %ux%u\n", size.width, size.height);

    /* TODO: attach(parent_hwnd) -> set_bounds -> show -> message loop. */

    if (v->destroy_editor)
        v->destroy_editor(inst.handle(), ed.handle);
    return true;
}

} // namespace dauxhost

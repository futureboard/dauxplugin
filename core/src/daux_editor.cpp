/*
 * daux_editor.cpp - Host-side editor helper utilities.
 *
 * Host-side C++ helper (daux_core). At present the editor lifecycle is driven
 * entirely through the daux_editor_vtable C ABI, so there is little C++ glue
 * needed here. This translation unit exists (per the planned core layout) as
 * the home for future helpers such as:
 *
 *   - cross-process window reparenting for DAUxHost --mode=editor,
 *   - a default "no editor" vtable for headless builds,
 *   - DPI / bounds negotiation helpers.
 *
 * For now it provides a tiny helper to drive an editor through attach/show.
 */
#include "daux_core.hpp"
#include "daux_editor.h"

namespace daux {

/* Convenience: attach an editor to a parent window and show it, returning the
 * first non-OK result (or DAUX_OK). The host normally orchestrates this itself,
 * but tests and the simple DAUxHost editor mode reuse this. */
daux_result editor_attach_and_show(const daux_editor& ed, daux_native_window parent) {
    if (!ed.handle || !ed.vtable) return DAUX_ERR_INVALID_ARG;
    const daux_editor_vtable* v = ed.vtable;

    if (v->attach) {
        daux_result r = v->attach(ed.handle, parent);
        if (r != DAUX_OK) return r;
    }
    if (v->show) {
        daux_result r = v->show(ed.handle);
        if (r != DAUX_OK) return r;
    }
    return DAUX_OK;
}

} // namespace daux

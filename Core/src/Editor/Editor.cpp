/*
 * Editor.cpp - Host-side editor helper utilities.
 */
#include <DAUx/Core.hpp>

namespace daux {

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

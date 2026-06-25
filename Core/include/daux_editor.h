/*
 * daux_editor.h - Plugin editor (GUI) interface.
 *
 * Part of "DAUx Plugin Core" / "DAUx Plugin ABI".
 *
 * The editor is intentionally decoupled from the audio-processing vtable so a
 * plugin can be processed headless (no GUI) and so editors can be implemented
 * by different toolkits in different languages (Rust GPUI, C# Avalonia, native
 * C++). The host obtains a daux_editor_vtable from a plugin instance via
 * daux_plugin_vtable.create_editor().
 *
 * Window model
 * ------------
 * The host owns a parent native window (HWND on Windows) and asks the editor to
 * attach into it. The editor creates its child surface and renders there. When
 * the editor lives in a separate process (DAUxHost.exe --mode=editor) the
 * parent handle is reparented across processes by the host; from the plugin's
 * point of view the contract is identical.
 */
#ifndef DAUX_EDITOR_H
#define DAUX_EDITOR_H

#include "daux_types.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct daux_size {
    uint32_t width;
    uint32_t height;
} daux_size;

typedef struct daux_rect {
    int32_t  x;
    int32_t  y;
    uint32_t width;
    uint32_t height;
} daux_rect;

/* Per-editor-instance vtable. The first argument is always the editor handle
 * returned by daux_plugin_vtable.create_editor(). */
typedef struct daux_editor_vtable {
    uint32_t struct_size;

    /* Attach the editor's content into a host-provided native parent window.
     * Must be called before show(). */
    daux_result (DAUX_CALL *attach)(daux_editor_handle ed, daux_native_window parent);

    /* Detach from the current parent. Safe to re-attach afterwards. */
    daux_result (DAUX_CALL *detach)(daux_editor_handle ed);

    /* Preferred initial size in pixels. */
    daux_result (DAUX_CALL *get_preferred_size)(daux_editor_handle ed, daux_size* out_size);

    /* Host-driven resize. The editor should lay out to the new bounds. */
    daux_result (DAUX_CALL *set_bounds)(daux_editor_handle ed, const daux_rect* bounds);

    daux_result (DAUX_CALL *show)(daux_editor_handle ed);
    daux_result (DAUX_CALL *hide)(daux_editor_handle ed);

    /* Optional periodic tick from the host UI thread (animations, meters).
     * May be NULL if the editor does not need it. */
    void        (DAUX_CALL *idle)(daux_editor_handle ed);

    void* reserved[6];
} daux_editor_vtable;

/* What create_editor returns: the instance handle + its vtable. */
typedef struct daux_editor {
    daux_editor_handle        handle;
    const daux_editor_vtable* vtable;
} daux_editor;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_EDITOR_H */

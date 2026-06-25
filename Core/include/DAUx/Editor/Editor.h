/*
 * DAUx/Editor/Editor.h - Plugin editor (GUI) interface.
 */
#ifndef DAUX_EDITOR_EDITOR_H
#define DAUX_EDITOR_EDITOR_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/Abi/Types.h>
#include <DAUx/Editor/EditorBounds.h>
#include <DAUx/Editor/NativeWindow.h>

#ifdef __cplusplus
extern "C" {
#endif

struct daux_host_callbacks;

typedef struct daux_editor daux_editor;

typedef daux_result (DAUX_CALL *DAUxEditorCreateFn)(
    daux_plugin_handle self, const struct daux_host_callbacks* host, daux_editor* out_editor);
typedef daux_result (DAUX_CALL *DAUxEditorDestroyFn)(
    daux_plugin_handle self, daux_editor_handle editor);
typedef daux_result (DAUX_CALL *DAUxEditorGetPreferredSizeFn)(
    daux_editor_handle ed, daux_size* out_size);
typedef daux_result (DAUX_CALL *DAUxEditorSetParentFn)(
    daux_editor_handle ed, daux_native_window parent);
typedef daux_result (DAUX_CALL *DAUxEditorSetBoundsFn)(
    daux_editor_handle ed, const daux_rect* bounds);
typedef daux_result (DAUX_CALL *DAUxEditorShowFn)(daux_editor_handle ed);
typedef daux_result (DAUX_CALL *DAUxEditorHideFn)(daux_editor_handle ed);
typedef void        (DAUX_CALL *DAUxEditorTickFn)(daux_editor_handle ed);

typedef struct daux_editor_vtable {
    uint32_t struct_size;

    daux_result (DAUX_CALL *attach)(daux_editor_handle ed, daux_native_window parent);
    daux_result (DAUX_CALL *detach)(daux_editor_handle ed);
    daux_result (DAUX_CALL *get_preferred_size)(daux_editor_handle ed, daux_size* out_size);
    daux_result (DAUX_CALL *set_bounds)(daux_editor_handle ed, const daux_rect* bounds);
    daux_result (DAUX_CALL *show)(daux_editor_handle ed);
    daux_result (DAUX_CALL *hide)(daux_editor_handle ed);
    void        (DAUX_CALL *idle)(daux_editor_handle ed);

    void* reserved[6];
} daux_editor_vtable;

struct daux_editor {
    daux_editor_handle        handle;
    const daux_editor_vtable* vtable;
};

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_EDITOR_EDITOR_H */

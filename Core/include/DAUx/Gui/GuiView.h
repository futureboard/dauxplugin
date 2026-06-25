#ifndef DAUX_GUI_GUI_VIEW_H
#define DAUX_GUI_GUI_VIEW_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/Editor/EditorBounds.h>
#include <DAUx/Editor/NativeWindow.h>
#include <DAUx/Gui/GuiFramework.h>
#include <DAUx/Gui/GuiMessage.h>
#include <DAUx/Gui/GuiSurface.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxGuiView DAUxGuiView;

typedef struct DAUxGuiViewInfo {
    DAUxGuiFramework framework;
    const char*      framework_name;
    uint32_t         supports_embed;
    uint32_t         supports_external_window;
    uint32_t         supports_resize;
    uint32_t         supports_hidpi;
    uint32_t         preferred_width;
    uint32_t         preferred_height;
    DAUxEditorMode   default_mode;
} DAUxGuiViewInfo;

typedef daux_result (DAUX_CALL *DAUxGuiGetViewInfoFn)(DAUxGuiView* view, DAUxGuiViewInfo* out);
typedef daux_result (DAUX_CALL *DAUxGuiCreateViewFn)(void* controller, DAUxGuiView** out_view);
typedef daux_result (DAUX_CALL *DAUxGuiDestroyViewFn)(DAUxGuiView* view);
typedef daux_result (DAUX_CALL *DAUxGuiAttachToParentFn)(DAUxGuiView* view, DAUxNativeWindowHandle parent);
typedef daux_result (DAUX_CALL *DAUxGuiDetachFromParentFn)(DAUxGuiView* view);
typedef daux_result (DAUX_CALL *DAUxGuiShowFn)(DAUxGuiView* view);
typedef daux_result (DAUX_CALL *DAUxGuiHideFn)(DAUxGuiView* view);
typedef daux_result (DAUX_CALL *DAUxGuiResizeFn)(DAUxGuiView* view, const DAUxEditorBounds* bounds);
typedef daux_result (DAUX_CALL *DAUxGuiSetScaleFactorFn)(DAUxGuiView* view, float scale);
typedef daux_result (DAUX_CALL *DAUxGuiFocusFn)(DAUxGuiView* view);
typedef daux_result (DAUX_CALL *DAUxGuiBlurFn)(DAUxGuiView* view);
typedef daux_result (DAUX_CALL *DAUxGuiIdleFn)(DAUxGuiView* view);
typedef daux_result (DAUX_CALL *DAUxGuiSendMessageFn)(DAUxGuiView* view, const DAUxGuiMessage* msg);
typedef daux_result (DAUX_CALL *DAUxGuiReceiveMessageFn)(DAUxGuiView* view, DAUxGuiMessage* msg);

typedef struct DAUxGuiViewVTable {
    uint32_t struct_size;
    DAUxGuiGetViewInfoFn       get_view_info;
    DAUxGuiAttachToParentFn    attach_to_parent;
    DAUxGuiDetachFromParentFn  detach_from_parent;
    DAUxGuiShowFn              show;
    DAUxGuiHideFn              hide;
    DAUxGuiResizeFn            resize;
    DAUxGuiSetScaleFactorFn    set_scale_factor;
    DAUxGuiFocusFn             focus;
    DAUxGuiBlurFn              blur;
    DAUxGuiIdleFn              idle;
    DAUxGuiSendMessageFn       send_message;
    DAUxGuiReceiveMessageFn    receive_message;
    void* reserved[4];
} DAUxGuiViewVTable;

struct DAUxGuiView {
    void*                    handle;
    const DAUxGuiViewVTable* vtable;
    DAUxGuiViewInfo          info;
};

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_GUI_GUI_VIEW_H */

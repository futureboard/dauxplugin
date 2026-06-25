#ifndef DAUX_GUI_GUI_SURFACE_H
#define DAUX_GUI_GUI_SURFACE_H

#include <stdint.h>

#include <DAUx/Editor/NativeWindow.h>
#include <DAUx/Gui/GuiFramework.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxGuiSurface {
    DAUxNativeWindowHandle native_handle;
    DAUxGuiFramework       framework;
    DAUxEditorMode         mode;
    uint32_t               width;
    uint32_t               height;
    float                  scale_factor;
} DAUxGuiSurface;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_GUI_GUI_SURFACE_H */

#ifndef DAUX_GUI_GUI_FRAMEWORK_H
#define DAUX_GUI_GUI_FRAMEWORK_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum DAUxGuiFramework {
    DAUX_GUI_FRAMEWORK_UNKNOWN   = 0,
    DAUX_GUI_FRAMEWORK_NATIVE    = 1,
    DAUX_GUI_FRAMEWORK_WIN32     = 2,
    DAUX_GUI_FRAMEWORK_COCOA     = 3,
    DAUX_GUI_FRAMEWORK_X11       = 4,
    DAUX_GUI_FRAMEWORK_WAYLAND   = 5,
    DAUX_GUI_FRAMEWORK_GPUI      = 6,
    DAUX_GUI_FRAMEWORK_AVALONIA  = 7,
    DAUX_GUI_FRAMEWORK_QT        = 8,
    DAUX_GUI_FRAMEWORK_WEBVIEW   = 9,
    DAUX_GUI_FRAMEWORK_SWIFTUI   = 10
} DAUxGuiFramework;

typedef enum DAUxEditorMode {
    DAUX_EDITOR_MODE_EMBEDDED = 1,
    DAUX_EDITOR_MODE_EXTERNAL = 2
} DAUxEditorMode;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_GUI_GUI_FRAMEWORK_H */

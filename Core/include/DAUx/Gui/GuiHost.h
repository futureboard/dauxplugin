#ifndef DAUX_GUI_GUI_HOST_H
#define DAUX_GUI_GUI_HOST_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Gui/GuiMessage.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxGuiHost {
    void (DAUX_CALL *post_gui_message)(void* host_ctx, const DAUxGuiMessage* msg);
    void (DAUX_CALL *request_external_window)(void* host_ctx, uint32_t width, uint32_t height);
    void* host_ctx;
} DAUxGuiHost;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_GUI_GUI_HOST_H */

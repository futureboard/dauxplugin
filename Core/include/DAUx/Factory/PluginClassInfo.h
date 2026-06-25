#ifndef DAUX_FACTORY_PLUGIN_CLASS_INFO_H
#define DAUX_FACTORY_PLUGIN_CLASS_INFO_H

#include <stdint.h>

#include <DAUx/Abi/Version.h>
#include <DAUx/Gui/GuiFramework.h>
#include <DAUx/Plugin/Category.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxPluginClassInfo {
    const char*        class_id;
    const char*        name;
    const char*        vendor;
    DAUxVersion        version;
    DAUxPluginCategory category;
    uint32_t           capabilities;
    DAUxGuiFramework   gui_framework;
    DAUxEditorMode     default_editor_mode;
    uint32_t           audio_input_bus_count;
    uint32_t           audio_output_bus_count;
} DAUxPluginClassInfo;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_FACTORY_PLUGIN_CLASS_INFO_H */

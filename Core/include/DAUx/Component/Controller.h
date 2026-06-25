#ifndef DAUX_COMPONENT_CONTROLLER_H
#define DAUX_COMPONENT_CONTROLLER_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/Abi/Types.h>
#include <DAUx/Host/HostCallbacks.h>
#include <DAUx/Component/ComponentState.h>
#include <DAUx/Editor/Editor.h>
#include <DAUx/Gui/GuiView.h>
#include <DAUx/Parameter/Parameter.h>
#include <DAUx/Parameter/ParameterInfo.h>
#include <DAUx/Program/Program.h>
#include <DAUx/Program/ProgramList.h>
#include <DAUx/Program/Preset.h>
#include <DAUx/State/State.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxController DAUxController;
struct DAUxHostServices;

typedef daux_result (DAUX_CALL *DAUxControllerGetParameterCountFn)(DAUxController* ctrl, uint32_t* out);
typedef daux_result (DAUX_CALL *DAUxControllerGetParameterInfoFn)(
    DAUxController* ctrl, uint32_t index, daux_param_info* out);
typedef daux_result (DAUX_CALL *DAUxControllerGetParameterValueFn)(
    DAUxController* ctrl, daux_param_id id, double* out);
typedef daux_result (DAUX_CALL *DAUxControllerSetParameterValueFn)(
    DAUxController* ctrl, daux_param_id id, double value);
typedef daux_result (DAUX_CALL *DAUxControllerNormalizedToPlainFn)(
    DAUxController* ctrl, daux_param_id id, double normalized, double* out);
typedef daux_result (DAUX_CALL *DAUxControllerPlainToNormalizedFn)(
    DAUxController* ctrl, daux_param_id id, double plain, double* out);
typedef daux_result (DAUX_CALL *DAUxControllerValueToTextFn)(
    DAUxController* ctrl, daux_param_id id, double plain, char* buf, uint32_t buf_size);
typedef daux_result (DAUX_CALL *DAUxControllerTextToValueFn)(
    DAUxController* ctrl, daux_param_id id, const char* text, double* out);
typedef daux_result (DAUX_CALL *DAUxControllerBeginEditFn)(DAUxController* ctrl, daux_param_id id);
typedef daux_result (DAUX_CALL *DAUxControllerPerformEditFn)(DAUxController* ctrl, daux_param_id id, double plain);
typedef daux_result (DAUX_CALL *DAUxControllerEndEditFn)(DAUxController* ctrl, daux_param_id id);

typedef daux_result (DAUX_CALL *DAUxControllerGetGuiViewInfoFn)(DAUxController* ctrl, DAUxGuiViewInfo* out);
typedef daux_result (DAUX_CALL *DAUxControllerCreateGuiViewFn)(
    DAUxController* ctrl, const struct DAUxHostServices* host, DAUxGuiView** out);
typedef daux_result (DAUX_CALL *DAUxControllerDestroyGuiViewFn)(DAUxController* ctrl, DAUxGuiView* view);

typedef daux_result (DAUX_CALL *DAUxControllerGetProgramListCountFn)(DAUxController* ctrl, uint32_t* out);
typedef daux_result (DAUX_CALL *DAUxControllerGetProgramListInfoFn)(
    DAUxController* ctrl, uint32_t index, DAUxProgramListInfo* out);
typedef daux_result (DAUX_CALL *DAUxControllerGetProgramCountFn)(DAUxController* ctrl, uint32_t list, uint32_t* out);
typedef daux_result (DAUX_CALL *DAUxControllerGetProgramNameFn)(
    DAUxController* ctrl, uint32_t list, uint32_t index, char* buf, uint32_t buf_size);
typedef daux_result (DAUX_CALL *DAUxControllerSelectProgramFn)(
    DAUxController* ctrl, uint32_t list, uint32_t index);
typedef daux_result (DAUX_CALL *DAUxControllerLoadPresetFn)(DAUxController* ctrl, const DAUxPreset* preset);
typedef daux_result (DAUX_CALL *DAUxControllerSavePresetFn)(DAUxController* ctrl, DAUxPreset* preset);

typedef struct DAUxControllerVTable {
    uint32_t struct_size;

    DAUxControllerGetParameterCountFn     get_parameter_count;
    DAUxControllerGetParameterInfoFn      get_parameter_info;
    DAUxControllerGetParameterValueFn     get_parameter_value;
    DAUxControllerSetParameterValueFn     set_parameter_value;
    DAUxControllerNormalizedToPlainFn   normalized_to_plain;
    DAUxControllerPlainToNormalizedFn     plain_to_normalized;
    DAUxControllerValueToTextFn           value_to_text;
    DAUxControllerTextToValueFn           text_to_value;
    DAUxControllerBeginEditFn             begin_edit;
    DAUxControllerPerformEditFn           perform_edit;
    DAUxControllerEndEditFn               end_edit;

    DAUxPluginGetStateFn get_state;
    DAUxPluginSetStateFn set_state;
    DAUxGetControllerStateFn get_controller_state;
    DAUxSetControllerStateFn set_controller_state;

    DAUxControllerGetGuiViewInfoFn   get_gui_view_info;
    DAUxControllerCreateGuiViewFn    create_gui_view;
    DAUxControllerDestroyGuiViewFn destroy_gui_view;

    DAUxControllerGetProgramListCountFn get_program_list_count;
    DAUxControllerGetProgramListInfoFn  get_program_list_info;
    DAUxControllerGetProgramCountFn     get_program_count;
    DAUxControllerGetProgramNameFn      get_program_name;
    DAUxControllerSelectProgramFn       select_program;
    DAUxControllerLoadPresetFn          load_preset;
    DAUxControllerSavePresetFn          save_preset;

    /* Legacy editor bridge (maps to daux_editor). */
    daux_result (DAUX_CALL *create_editor)(DAUxController* ctrl,
                                            const struct daux_host_callbacks* host,
                                            daux_editor* out);
    daux_result (DAUX_CALL *destroy_editor)(DAUxController* ctrl, daux_editor_handle ed);

    void* reserved[8];
} DAUxControllerVTable;

struct DAUxController {
    void*                       handle;
    const DAUxControllerVTable* vtable;
};

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_COMPONENT_CONTROLLER_H */

/*
 * DAUx/Parameter/Parameter.h - Parameter callback typedefs.
 */
#ifndef DAUX_PARAMETER_PARAMETER_H
#define DAUX_PARAMETER_PARAMETER_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/Abi/Types.h>
#include <DAUx/Parameter/ParameterInfo.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef uint32_t (DAUX_CALL *DAUxPluginGetParameterCountFn)(daux_plugin_handle self);
typedef daux_result (DAUX_CALL *DAUxPluginGetParameterInfoFn)(
    daux_plugin_handle self, uint32_t index, daux_param_info* out_info);
typedef daux_result (DAUX_CALL *DAUxPluginGetParameterValueFn)(
    daux_plugin_handle self, daux_param_id id, double* out_value);
typedef daux_result (DAUX_CALL *DAUxPluginSetParameterValueFn)(
    daux_plugin_handle self, daux_param_id id, double value);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PARAMETER_PARAMETER_H */

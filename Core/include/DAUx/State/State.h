/*
 * DAUx/State/State.h - State persistence callback typedefs.
 */
#ifndef DAUX_STATE_STATE_H
#define DAUX_STATE_STATE_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/Abi/Types.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef daux_result (DAUX_CALL *DAUxPluginGetStateFn)(
    daux_plugin_handle self, void* out_buf, uint32_t buf_size, uint32_t* out_size);
typedef daux_result (DAUX_CALL *DAUxPluginSetStateFn)(
    daux_plugin_handle self, const void* data, uint32_t size);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_STATE_STATE_H */

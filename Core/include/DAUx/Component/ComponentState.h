#ifndef DAUX_COMPONENT_COMPONENT_STATE_H
#define DAUX_COMPONENT_COMPONENT_STATE_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/State/StateBlob.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef daux_result (DAUX_CALL *DAUxGetComponentStateFn)(
    void* component, DAUxStateBlob* out_state);
typedef daux_result (DAUX_CALL *DAUxSetComponentStateFn)(
    void* component, const DAUxStateBlob* state);
typedef daux_result (DAUX_CALL *DAUxGetProcessorStateFn)(
    void* processor, DAUxStateBlob* out_state);
typedef daux_result (DAUX_CALL *DAUxSetProcessorStateFn)(
    void* processor, const DAUxStateBlob* state);
typedef daux_result (DAUX_CALL *DAUxGetControllerStateFn)(
    void* controller, DAUxStateBlob* out_state);
typedef daux_result (DAUX_CALL *DAUxSetControllerStateFn)(
    void* controller, const DAUxStateBlob* state);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_COMPONENT_COMPONENT_STATE_H */

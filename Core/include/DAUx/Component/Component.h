#ifndef DAUX_COMPONENT_COMPONENT_H
#define DAUX_COMPONENT_COMPONENT_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/Component/Controller.h>
#include <DAUx/Component/Processor.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxComponent DAUxComponent;
struct DAUxHostServices;

typedef daux_result (DAUX_CALL *DAUxComponentCreateFn)(
    const struct DAUxHostServices* host, DAUxComponent** out);
typedef daux_result (DAUX_CALL *DAUxComponentDestroyFn)(DAUxComponent* component);
typedef daux_result (DAUX_CALL *DAUxComponentGetProcessorFn)(
    DAUxComponent* component, DAUxProcessor** out);
typedef daux_result (DAUX_CALL *DAUxComponentGetControllerFn)(
    DAUxComponent* component, DAUxController** out);

typedef struct DAUxComponentVTable {
    uint32_t struct_size;
    DAUxComponentCreateFn       create;
    DAUxComponentDestroyFn      destroy;
    DAUxComponentGetProcessorFn get_processor;
    DAUxComponentGetControllerFn get_controller;
    void* reserved[8];
} DAUxComponentVTable;

struct DAUxComponent {
    void*                      handle;
    const DAUxComponentVTable* vtable;
    DAUxProcessor              processor;
    DAUxController             controller;
};

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_COMPONENT_COMPONENT_H */

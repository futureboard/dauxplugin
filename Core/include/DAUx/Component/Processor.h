#ifndef DAUX_COMPONENT_PROCESSOR_H
#define DAUX_COMPONENT_PROCESSOR_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/Component/ProcessData.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxProcessor DAUxProcessor;

typedef daux_result (DAUX_CALL *DAUxProcessorPrepareFn)(
    DAUxProcessor* processor, double sample_rate, uint32_t max_block_size);
typedef daux_result (DAUX_CALL *DAUxProcessorSetActiveFn)(
    DAUxProcessor* processor, uint32_t active);
typedef daux_result (DAUX_CALL *DAUxProcessorProcessFn)(
    DAUxProcessor* processor, DAUxProcessData* data);
typedef daux_result (DAUX_CALL *DAUxProcessorResetFn)(DAUxProcessor* processor);

typedef struct DAUxProcessorVTable {
    uint32_t struct_size;
    DAUxProcessorPrepareFn   prepare;
    DAUxProcessorSetActiveFn set_active;
    DAUxProcessorProcessFn   process;
    DAUxProcessorResetFn     reset;
    void* reserved[8];
} DAUxProcessorVTable;

struct DAUxProcessor {
    void*                      handle;
    const DAUxProcessorVTable* vtable;
};

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_COMPONENT_PROCESSOR_H */

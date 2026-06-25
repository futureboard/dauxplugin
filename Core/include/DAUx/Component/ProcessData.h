#ifndef DAUX_COMPONENT_PROCESS_DATA_H
#define DAUX_COMPONENT_PROCESS_DATA_H

#include <stdint.h>

#include <DAUx/Audio/AudioBus.h>
#include <DAUx/Audio/ProcessContext.h>
#include <DAUx/Automation/ParameterChangeQueue.h>
#include <DAUx/Event/EventQueue.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxProcessData {
    DAUxAudioBus*              inputs;
    uint32_t                   input_bus_count;
    DAUxAudioBus*              outputs;
    uint32_t                   output_bus_count;
    DAUxEventQueue*            input_events;
    DAUxEventQueue*            output_events;
    DAUxParameterChangeQueue*  input_parameter_changes;
    DAUxParameterChangeQueue*  output_parameter_changes;
    DAUxProcessContext         context;
    uint32_t                   frame_count;
} DAUxProcessData;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_COMPONENT_PROCESS_DATA_H */

/*
 * DAUx/Audio/AudioBus.h - Audio bus buffers and IO layout descriptors.
 */
#ifndef DAUX_AUDIO_AUDIO_BUS_H
#define DAUX_AUDIO_AUDIO_BUS_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/Audio/ChannelLayout.h>
#include <DAUx/Audio/SampleFormat.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxAudioBus {
    void**            channels;
    uint32_t          channel_count;
    uint32_t          frame_count;
    DAUxSampleFormat  sample_format;
} DAUxAudioBus;

typedef enum DAUxBusDirection {
    DAUX_BUS_INPUT  = 0,
    DAUX_BUS_OUTPUT = 1
} DAUxBusDirection;

typedef enum DAUxBusType {
    DAUX_BUS_AUDIO     = 0,
    DAUX_BUS_EVENT     = 1,
    DAUX_BUS_SIDECHAIN = 2
} DAUxBusType;

#define DAUX_BUS_FLAG_ACTIVE       (1u << 0)
#define DAUX_BUS_FLAG_SIDECHAIN    (1u << 1)
#define DAUX_BUS_FLAG_MAIN         (1u << 2)

typedef struct DAUxBusInfo {
    uint32_t           index;
    DAUxBusDirection   direction;
    DAUxBusType        type;
    const char*        name;
    uint32_t           channel_count;
    DAUxChannelLayout  channel_layout;
    uint32_t           flags;
} DAUxBusInfo;

typedef daux_result (DAUX_CALL *DAUxGetBusCountFn)(
    void* self, DAUxBusDirection direction, uint32_t* out_count);
typedef daux_result (DAUX_CALL *DAUxGetBusInfoFn)(
    void* self, DAUxBusDirection direction, uint32_t index, DAUxBusInfo* out);
typedef daux_result (DAUX_CALL *DAUxActivateBusFn)(
    void* self, DAUxBusDirection direction, uint32_t index, uint32_t active);
typedef daux_result (DAUX_CALL *DAUxSetBusArrangementFn)(
    void* self, DAUxBusDirection direction, uint32_t index, DAUxChannelLayout layout);
typedef daux_result (DAUX_CALL *DAUxGetBusArrangementFn)(
    void* self, DAUxBusDirection direction, uint32_t index, DAUxChannelLayout* out);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_AUDIO_AUDIO_BUS_H */

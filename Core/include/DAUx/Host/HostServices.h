/*
 * DAUx/Host/HostServices.h - Extended host services (non-realtime unless noted).
 */
#ifndef DAUX_HOST_HOST_SERVICES_H
#define DAUX_HOST_HOST_SERVICES_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Types.h>
#include <DAUx/Abi/Version.h>
#include <DAUx/Gui/GuiMessage.h>
#include <DAUx/Host/HostCallbacks.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxTransportState {
    double   bpm;
    uint64_t sample_position;
    uint32_t playing;
    uint32_t recording;
    uint32_t looping;
} DAUxTransportState;

typedef struct DAUxHostServices {
    uint32_t struct_size;
    uint32_t abi_version;
    daux_host_context context;

    /* --- Legacy callbacks (non-realtime) --- */
    daux_host_callbacks legacy;

    /* --- Non-realtime host services --- */
    void (DAUX_CALL *log)(daux_host_context ctx, int32_t level, const char* msg);
    void* (DAUX_CALL *allocate)(daux_host_context ctx, uint32_t size);
    void  (DAUX_CALL *free)(daux_host_context ctx, void* ptr);

    void (DAUX_CALL *parameter_changed)(daux_host_context ctx, daux_param_id id, double normalized);
    void (DAUX_CALL *begin_edit)(daux_host_context ctx, daux_param_id id);
    void (DAUX_CALL *perform_edit)(daux_host_context ctx, daux_param_id id, double plain);
    void (DAUX_CALL *end_edit)(daux_host_context ctx, daux_param_id id);

    void (DAUX_CALL *restart_component)(daux_host_context ctx, uint32_t flags);
    void (DAUX_CALL *request_process)(daux_host_context ctx);
    void (DAUX_CALL *request_resize)(daux_host_context ctx, uint32_t width, uint32_t height);
    void (DAUX_CALL *request_external_window)(daux_host_context ctx, uint32_t width, uint32_t height);

    double   (DAUX_CALL *get_sample_rate)(daux_host_context ctx);
    uint32_t (DAUX_CALL *get_block_size)(daux_host_context ctx);
    daux_result (DAUX_CALL *get_transport_state)(daux_host_context ctx, DAUxTransportState* out);
    double   (DAUX_CALL *get_tempo)(daux_host_context ctx);
    void     (DAUX_CALL *mark_state_dirty)(daux_host_context ctx);

    void (DAUX_CALL *post_gui_message)(daux_host_context ctx, const DAUxGuiMessage* msg);

    void* reserved[8];
} DAUxHostServices;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_HOST_HOST_SERVICES_H */

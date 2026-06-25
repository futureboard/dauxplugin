#ifndef DAUX_GUI_GUI_MESSAGE_H
#define DAUX_GUI_GUI_MESSAGE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxGuiMessage {
    const char*    type;
    const uint8_t* payload;
    uint64_t       payload_size;
} DAUxGuiMessage;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_GUI_GUI_MESSAGE_H */

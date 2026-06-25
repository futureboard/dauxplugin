/*
 * DAUx/Plugin/Descriptor.h
 */
#ifndef DAUX_PLUGIN_DESCRIPTOR_H
#define DAUX_PLUGIN_DESCRIPTOR_H

#include <stdint.h>

#include <DAUx/Abi/Types.h>
#include <DAUx/Abi/Version.h>
#include <DAUx/Plugin/Category.h>
#include <DAUx/Plugin/Capabilities.h>

#ifdef __cplusplus
extern "C" {
#endif

struct DAUxPluginVTable;

/* Legacy descriptor (inline UTF-8 buffers; used by DAUxScan and existing plugins). */
typedef struct daux_plugin_descriptor {
    char     id[DAUX_ID_SIZE];
    char     name[DAUX_NAME_SIZE];
    char     vendor[DAUX_VENDOR_SIZE];

    uint32_t version;
    daux_plugin_category category;
    daux_plugin_caps     capabilities;

    uint32_t audio_input_buses;
    uint32_t audio_output_buses;
    uint32_t default_channels_per_bus;

    uint32_t reserved[8];
} daux_plugin_descriptor;

/* Future pointer-based descriptor (not yet used by the host loader). */
typedef struct DAUxPluginDescriptor {
    const char*              id;
    const char*              name;
    const char*              vendor;
    DAUxVersion              version;
    DAUxPluginCategory       category;
    uint32_t                 capabilities;
    uint32_t                 audio_input_bus_count;
    uint32_t                 audio_output_bus_count;
    const struct DAUxPluginVTable* vtable;
} DAUxPluginDescriptor;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PLUGIN_DESCRIPTOR_H */

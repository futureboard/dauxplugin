/*
 * DAUx/Plugin/EntryPoint.h - Plugin module entry symbols.
 */
#ifndef DAUX_PLUGIN_ENTRY_POINT_H
#define DAUX_PLUGIN_ENTRY_POINT_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Plugin/Descriptor.h>
#include <DAUx/Plugin/Plugin.h>

#ifdef __cplusplus
extern "C" {
#endif

struct DAUxPluginFactory;

#define DAUX_PLUGIN_ENTRY_SYMBOL "daux_plugin_entry"

#define DAUX_PLUGIN_ENTRYPOINT "daux_get_plugin_descriptor"
#define DAUX_PLUGIN_FACTORY_ENTRYPOINT "daux_get_plugin_factory"

typedef const daux_plugin_factory* (DAUX_CALL *daux_plugin_entry_fn)(uint32_t host_abi_version);

typedef const DAUxPluginDescriptor* (*DAUxGetPluginDescriptorFn)(void);

typedef const DAUxPluginFactory* (DAUX_CALL *daux_get_plugin_factory_fn)(void);

DAUX_API const daux_plugin_factory* DAUX_CALL daux_plugin_entry(uint32_t host_abi_version);

DAUX_API const DAUxPluginDescriptor* daux_get_plugin_descriptor(void);
DAUX_API const DAUxPluginFactory* daux_get_plugin_factory(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_PLUGIN_ENTRY_POINT_H */

#ifndef DAUX_FACTORY_PLUGIN_CLASS_REGISTRY_H
#define DAUX_FACTORY_PLUGIN_CLASS_REGISTRY_H

#include <stdint.h>

#include <DAUx/Factory/PluginClassInfo.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DAUxPluginClassRegistry {
    const DAUxPluginClassInfo* classes;
    uint32_t                   count;
} DAUxPluginClassRegistry;

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_FACTORY_PLUGIN_CLASS_REGISTRY_H */

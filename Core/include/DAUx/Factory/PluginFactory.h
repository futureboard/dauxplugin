#ifndef DAUX_FACTORY_PLUGIN_FACTORY_H
#define DAUX_FACTORY_PLUGIN_FACTORY_H

#include <stdint.h>

#include <DAUx/Abi/Export.h>
#include <DAUx/Abi/Result.h>
#include <DAUx/Component/Component.h>
#include <DAUx/Factory/PluginClassInfo.h>
#include <DAUx/Plugin/Descriptor.h>

#ifdef __cplusplus
extern "C" {
#endif

struct DAUxHostServices;

typedef struct DAUxPluginFactory {
    uint32_t struct_size;
    uint32_t abi_version;

    uint32_t (DAUX_CALL *get_class_count)(void);
    daux_result (DAUX_CALL *get_class_info)(uint32_t index, DAUxPluginClassInfo* out);
    daux_result (DAUX_CALL *create_component_by_class_id)(
        const char* class_id,
        const struct DAUxHostServices* host,
        DAUxComponent** out_component);

    /* Optional bridge for legacy hosts/scanners. */
    const daux_plugin_descriptor* (DAUX_CALL *get_legacy_descriptor)(void);

    void* reserved[8];
} DAUxPluginFactory;

#define DAUX_PLUGIN_FACTORY_SYMBOL "daux_get_plugin_factory"

typedef const DAUxPluginFactory* (DAUX_CALL *daux_get_plugin_factory_fn)(void);

DAUX_API const DAUxPluginFactory* daux_get_plugin_factory(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* DAUX_FACTORY_PLUGIN_FACTORY_H */

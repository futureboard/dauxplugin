# DAUx Plugin Factory

A single `.dauxplug` binary may host **multiple plugin classes** (different IDs,
categories, or GUI stacks) through the modern factory API.

## Entry points (priority)

1. `daux_get_plugin_factory()` — preferred when present (`DAUx/Factory/PluginFactory.h`)
2. `daux_plugin_entry()` — legacy single-factory path (still required for audio today)
3. `daux_get_plugin_descriptor()` — optional single-descriptor shortcut

`DAUxHost` / `DAUxScan` load the module, resolve the factory symbol if exported,
and fall back to the legacy descriptor from `daux_plugin_entry`.

## Factory vtable

```c
typedef struct DAUxPluginFactory {
    uint32_t (DAUX_CALL *get_class_count)(void);
    daux_result (DAUX_CALL *get_class_info)(uint32_t index, DAUxPluginClassInfo* out);
    daux_result (DAUX_CALL *create_component_by_class_id)(...);
    const daux_plugin_descriptor* (DAUX_CALL *get_legacy_descriptor)(void);
} DAUxPluginFactory;
```

`get_legacy_descriptor` bridges old scanners until all hosts use `get_class_info`.

## C++ wrapper

`DAUx::PluginFactory` in `DAUx/Runtime/PluginFactory.hpp` wraps the C factory for
host tools (class listing, future component creation).

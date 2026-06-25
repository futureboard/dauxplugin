/*
 * DAUx/Runtime/PluginLoader.hpp - Future dynamic loader skeleton.
 *
 * The production host still uses daux::PluginModule from the legacy C++ helper.
 */
#ifndef DAUX_RUNTIME_PLUGIN_LOADER_HPP
#define DAUX_RUNTIME_PLUGIN_LOADER_HPP

#include <string>

#include <DAUx/DAUx.h>
#include <DAUx/Runtime/PluginInstance.hpp>

namespace DAUx {

class PluginLoader {
public:
    /* TODO: dynamic loading via DAUxPluginDescriptor entry points. */
    PluginInstance load(const std::string& path);
};

} // namespace DAUx

#endif /* DAUX_RUNTIME_PLUGIN_LOADER_HPP */

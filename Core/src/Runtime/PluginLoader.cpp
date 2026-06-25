#include <DAUx/Runtime/PluginLoader.hpp>

namespace DAUx {

PluginInstance PluginLoader::load(const std::string& /*path*/) {
    /* TODO: dynamic loading for DAUxPluginDescriptor-based plugins. */
    return PluginInstance{};
}

} // namespace DAUx

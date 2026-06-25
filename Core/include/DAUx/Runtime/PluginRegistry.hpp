/*
 * DAUx/Runtime/PluginRegistry.hpp - Future plugin registry skeleton.
 */
#ifndef DAUX_RUNTIME_PLUGIN_REGISTRY_HPP
#define DAUX_RUNTIME_PLUGIN_REGISTRY_HPP

#include <string>
#include <string_view>
#include <vector>

#include <DAUx/DAUx.h>

namespace DAUx {

class PluginRegistry {
public:
    void registerPlugin(const DAUxPluginDescriptor* descriptor);
    const DAUxPluginDescriptor* findById(std::string_view id) const;
    std::vector<const DAUxPluginDescriptor*> list() const;

private:
    std::vector<const DAUxPluginDescriptor*> entries_;
};

} // namespace DAUx

#endif /* DAUX_RUNTIME_PLUGIN_REGISTRY_HPP */

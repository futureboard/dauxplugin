#include <DAUx/Runtime/PluginRegistry.hpp>

namespace DAUx {

void PluginRegistry::registerPlugin(const DAUxPluginDescriptor* descriptor) {
    if (descriptor) entries_.push_back(descriptor);
}

const DAUxPluginDescriptor* PluginRegistry::findById(std::string_view id) const {
    for (const auto* e : entries_) {
        if (e && e->id && id == e->id) return e;
    }
    return nullptr;
}

std::vector<const DAUxPluginDescriptor*> PluginRegistry::list() const {
    return entries_;
}

} // namespace DAUx

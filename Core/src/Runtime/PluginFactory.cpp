#include <DAUx/Runtime/PluginFactory.hpp>
#include <DAUx/Runtime/PluginComponent.hpp>

#include <string>

#include <DAUx/Core.hpp>

namespace DAUx {

PluginFactory::PluginFactory(const DAUxPluginFactory* factory) : factory_(factory) {}

uint32_t PluginFactory::class_count() const {
    if (!factory_ || !factory_->get_class_count) return 0;
    return factory_->get_class_count();
}

DAUxPluginClassInfo PluginFactory::class_info(uint32_t index) const {
    DAUxPluginClassInfo info{};
    if (!factory_ || !factory_->get_class_info) return info;
    if (factory_->get_class_info(index, &info) != DAUX_OK) return DAUxPluginClassInfo{};
    return info;
}

PluginComponent PluginFactory::create_component(std::string_view class_id,
                                                const DAUxHostServices* host) const {
    if (!factory_ || !factory_->create_component_by_class_id) {
        throw daux::DauxError("factory does not support create_component_by_class_id",
                              DAUX_ERR_NOT_SUPPORTED);
    }
    DAUxComponent* raw = nullptr;
    std::string id(class_id);
    daux_result r = factory_->create_component_by_class_id(id.c_str(), host, &raw);
    if (r != DAUX_OK || !raw) {
        throw daux::DauxError("create_component_by_class_id failed: " +
                              std::string(daux::result_to_string(r)), r);
    }
    return PluginComponent(raw);
}

} // namespace DAUx

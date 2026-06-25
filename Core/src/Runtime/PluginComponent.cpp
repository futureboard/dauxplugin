#include <DAUx/Runtime/PluginComponent.hpp>

namespace DAUx {

PluginComponent::PluginComponent(DAUxComponent* component) : component_(component) {}

PluginProcessor PluginComponent::processor() {
    DAUxProcessor* p = nullptr;
    if (component_ && component_->vtable && component_->vtable->get_processor) {
        component_->vtable->get_processor(component_, &p);
    }
    return PluginProcessor(p);
}

PluginController PluginComponent::controller() {
    DAUxController* c = nullptr;
    if (component_ && component_->vtable && component_->vtable->get_controller) {
        component_->vtable->get_controller(component_, &c);
    }
    return PluginController(c);
}

} // namespace DAUx

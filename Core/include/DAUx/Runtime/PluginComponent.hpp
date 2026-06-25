#ifndef DAUX_RUNTIME_PLUGIN_COMPONENT_HPP
#define DAUX_RUNTIME_PLUGIN_COMPONENT_HPP

#include <DAUx/DAUx.h>
#include <DAUx/Runtime/PluginController.hpp>
#include <DAUx/Runtime/PluginProcessor.hpp>

namespace DAUx {

class PluginComponent {
public:
    PluginComponent() = default;
    explicit PluginComponent(DAUxComponent* component);

    bool valid() const noexcept { return component_ != nullptr; }
    PluginProcessor processor();
    PluginController controller();

private:
    DAUxComponent* component_ = nullptr;
};

} // namespace DAUx

#endif

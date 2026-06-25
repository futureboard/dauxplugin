#ifndef DAUX_RUNTIME_PLUGIN_CONTROLLER_HPP
#define DAUX_RUNTIME_PLUGIN_CONTROLLER_HPP

#include <DAUx/DAUx.h>
#include <DAUx/Runtime/PluginEditor.hpp>

namespace DAUx {

class PluginController {
public:
    PluginController() = default;
    explicit PluginController(DAUxController* controller);

    bool valid() const noexcept { return controller_ != nullptr; }
    uint32_t parameter_count() const;
    daux_param_info parameter_info(uint32_t index) const;
    double parameter_value(daux_param_id id) const;
    void set_parameter_value(daux_param_id id, double value);
    DAUxGuiViewInfo gui_view_info() const;
    PluginEditor create_editor(const DAUxHostServices* host);

private:
    DAUxController* controller_ = nullptr;
};

} // namespace DAUx

#endif

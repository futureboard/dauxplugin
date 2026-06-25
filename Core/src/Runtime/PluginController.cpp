#include <DAUx/Runtime/PluginController.hpp>
#include <DAUx/Runtime/PluginEditor.hpp>

#include <DAUx/Core.hpp>

namespace DAUx {

PluginController::PluginController(DAUxController* controller) : controller_(controller) {}

uint32_t PluginController::parameter_count() const {
    if (!controller_ || !controller_->vtable || !controller_->vtable->get_parameter_count) return 0;
    uint32_t n = 0;
    controller_->vtable->get_parameter_count(controller_, &n);
    return n;
}

daux_param_info PluginController::parameter_info(uint32_t index) const {
    daux_param_info info{};
    if (!controller_ || !controller_->vtable || !controller_->vtable->get_parameter_info) return info;
    controller_->vtable->get_parameter_info(controller_, index, &info);
    return info;
}

double PluginController::parameter_value(daux_param_id id) const {
    double v = 0.0;
    if (!controller_ || !controller_->vtable || !controller_->vtable->get_parameter_value) return v;
    controller_->vtable->get_parameter_value(controller_, id, &v);
    return v;
}

void PluginController::set_parameter_value(daux_param_id id, double value) {
    if (!controller_ || !controller_->vtable || !controller_->vtable->set_parameter_value) return;
    controller_->vtable->set_parameter_value(controller_, id, value);
}

DAUxGuiViewInfo PluginController::gui_view_info() const {
    DAUxGuiViewInfo info{};
    if (!controller_ || !controller_->vtable || !controller_->vtable->get_gui_view_info) return info;
    controller_->vtable->get_gui_view_info(controller_, &info);
    return info;
}

PluginEditor PluginController::create_editor(const DAUxHostServices* host) {
    DAUxGuiView* view = nullptr;
    if (!controller_ || !controller_->vtable || !controller_->vtable->create_gui_view) {
        return PluginEditor{};
    }
    if (controller_->vtable->create_gui_view(controller_, host, &view) != DAUX_OK) {
        return PluginEditor{};
    }
    return PluginEditor(view, controller_);
}

} // namespace DAUx

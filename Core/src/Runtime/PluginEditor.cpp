#include <DAUx/Runtime/PluginEditor.hpp>

namespace DAUx {

PluginEditor::PluginEditor(DAUxGuiView* view, DAUxController* owner)
    : view_(view), owner_(owner) {}

DAUxGuiViewInfo PluginEditor::view_info() const {
    DAUxGuiViewInfo info{};
    if (!view_ || !view_->vtable || !view_->vtable->get_view_info) return info;
    view_->vtable->get_view_info(view_, &info);
    return info;
}

void PluginEditor::show() {
    if (view_ && view_->vtable && view_->vtable->show) view_->vtable->show(view_);
}

void PluginEditor::hide() {
    if (view_ && view_->vtable && view_->vtable->hide) view_->vtable->hide(view_);
}

void PluginEditor::attach(DAUxNativeWindowHandle parent) {
    if (view_ && view_->vtable && view_->vtable->attach_to_parent) {
        view_->vtable->attach_to_parent(view_, parent);
    }
}

void PluginEditor::resize(const DAUxEditorBounds& bounds) {
    if (view_ && view_->vtable && view_->vtable->resize) {
        view_->vtable->resize(view_, &bounds);
    }
}

} // namespace DAUx

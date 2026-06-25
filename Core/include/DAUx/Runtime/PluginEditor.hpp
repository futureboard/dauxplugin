#ifndef DAUX_RUNTIME_PLUGIN_EDITOR_HPP
#define DAUX_RUNTIME_PLUGIN_EDITOR_HPP

#include <DAUx/DAUx.h>

namespace DAUx {

class PluginEditor {
public:
    PluginEditor() = default;
    PluginEditor(DAUxGuiView* view, DAUxController* owner);

    bool valid() const noexcept { return view_ != nullptr; }
    DAUxGuiViewInfo view_info() const;
    void show();
    void hide();
    void attach(DAUxNativeWindowHandle parent);
    void resize(const DAUxEditorBounds& bounds);

private:
    DAUxGuiView*     view_ = nullptr;
    DAUxController*  owner_ = nullptr;
};

} // namespace DAUx

#endif

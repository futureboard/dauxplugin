#ifndef DAUX_RUNTIME_PLUGIN_FACTORY_HPP
#define DAUX_RUNTIME_PLUGIN_FACTORY_HPP

#include <string>
#include <string_view>
#include <vector>

#include <DAUx/DAUx.h>

namespace DAUx {

class PluginComponent;

class PluginFactory {
public:
    explicit PluginFactory(const DAUxPluginFactory* factory);

    bool valid() const noexcept { return factory_ != nullptr; }
    uint32_t class_count() const;
    DAUxPluginClassInfo class_info(uint32_t index) const;
    PluginComponent create_component(std::string_view class_id, const DAUxHostServices* host) const;

private:
    const DAUxPluginFactory* factory_;
};

} // namespace DAUx

#endif

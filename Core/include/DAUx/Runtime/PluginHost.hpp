#ifndef DAUX_RUNTIME_PLUGIN_HOST_HPP
#define DAUX_RUNTIME_PLUGIN_HOST_HPP

#include <DAUx/DAUx.h>

namespace DAUx {

class PluginHost {
public:
    explicit PluginHost(const DAUxHostServices* services = nullptr);
    const DAUxHostServices* services() const noexcept { return services_; }
private:
    const DAUxHostServices* services_;
};

} // namespace DAUx

#endif

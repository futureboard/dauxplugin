/*
 * DAUx/Runtime/PluginInstance.hpp - Future plugin instance wrapper skeleton.
 */
#ifndef DAUX_RUNTIME_PLUGIN_INSTANCE_HPP
#define DAUX_RUNTIME_PLUGIN_INSTANCE_HPP

#include <DAUx/DAUx.h>

namespace DAUx {

class PluginInstance {
public:
    PluginInstance() = default;
    PluginInstance(const DAUxPluginDescriptor* descriptor, DAUxPluginInstance* instance);

    bool valid() const noexcept { return instance_ != nullptr; }

    void prepare(double sample_rate, uint32_t max_block_size);
    void process(const daux_process_data& data);
    void reset();

private:
    const DAUxPluginDescriptor* descriptor_ = nullptr;
    DAUxPluginInstance*         instance_   = nullptr;
};

} // namespace DAUx

#endif /* DAUX_RUNTIME_PLUGIN_INSTANCE_HPP */

#include <DAUx/Runtime/PluginInstance.hpp>

namespace DAUx {

PluginInstance::PluginInstance(const DAUxPluginDescriptor* descriptor,
                               DAUxPluginInstance* instance)
    : descriptor_(descriptor), instance_(instance) {}

void PluginInstance::prepare(double /*sample_rate*/, uint32_t /*max_block_size*/) {
    /* TODO: invoke DAUxPluginVTable::prepare when the new loader is wired. */
}

void PluginInstance::process(const daux_process_data& /*data*/) {
    /* TODO */
}

void PluginInstance::reset() {
    /* TODO */
}

} // namespace DAUx

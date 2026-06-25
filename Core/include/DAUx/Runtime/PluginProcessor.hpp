#ifndef DAUX_RUNTIME_PLUGIN_PROCESSOR_HPP
#define DAUX_RUNTIME_PLUGIN_PROCESSOR_HPP

#include <DAUx/DAUx.h>

namespace DAUx {

class PluginProcessor {
public:
    PluginProcessor() = default;
    explicit PluginProcessor(DAUxProcessor* processor);

    bool valid() const noexcept { return processor_ != nullptr; }
    void prepare(double sample_rate, uint32_t max_block);
    void set_active(bool active);
    void process(DAUxProcessData& data);
    void reset();

private:
    DAUxProcessor* processor_ = nullptr;
};

} // namespace DAUx

#endif

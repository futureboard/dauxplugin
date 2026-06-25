#include <DAUx/Runtime/PluginProcessor.hpp>

#include <DAUx/Core.hpp>

namespace DAUx {

PluginProcessor::PluginProcessor(DAUxProcessor* processor) : processor_(processor) {}

void PluginProcessor::prepare(double sample_rate, uint32_t max_block) {
    if (!processor_ || !processor_->vtable || !processor_->vtable->prepare) return;
    daux_result r = processor_->vtable->prepare(processor_, sample_rate, max_block);
    if (r != DAUX_OK) {
        throw daux::DauxError(std::string("processor prepare: ") + daux::result_to_string(r), r);
    }
}

void PluginProcessor::set_active(bool active) {
    if (!processor_ || !processor_->vtable || !processor_->vtable->set_active) return;
    processor_->vtable->set_active(processor_, active ? 1u : 0u);
}

void PluginProcessor::process(DAUxProcessData& data) {
    if (!processor_ || !processor_->vtable || !processor_->vtable->process) return;
    daux_result r = processor_->vtable->process(processor_, &data);
    if (r != DAUX_OK) {
        throw daux::DauxError(std::string("processor process: ") + daux::result_to_string(r), r);
    }
}

void PluginProcessor::reset() {
    if (!processor_ || !processor_->vtable || !processor_->vtable->reset) return;
    processor_->vtable->reset(processor_);
}

} // namespace DAUx

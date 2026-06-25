/*
 * DAUx/Core.hpp - C++ convenience header (legacy host helpers + future runtime).
 *
 * Plugins must NOT include this header.
 */
#ifndef DAUX_CORE_HPP
#define DAUX_CORE_HPP

#include <DAUx/DAUx.h>

#include <DAUx/Runtime/PluginRegistry.hpp>
#include <DAUx/Runtime/PluginInstance.hpp>
#include <DAUx/Runtime/PluginLoader.hpp>

#include <string>
#include <stdexcept>
#include <vector>
#include <cstdint>

namespace daux {

const char* result_to_string(daux_result r) noexcept;
bool abi_compatible(uint32_t plugin_abi_version) noexcept;
std::string version_to_string(uint32_t packed);

class DauxError : public std::runtime_error {
public:
    explicit DauxError(const std::string& what, daux_result code = DAUX_ERR_UNKNOWN)
        : std::runtime_error(what), code_(code) {}
    daux_result code() const noexcept { return code_; }
private:
    daux_result code_;
};

class PluginInstance;

class PluginModule {
public:
    explicit PluginModule(const std::string& path);
    ~PluginModule();

    PluginModule(const PluginModule&) = delete;
    PluginModule& operator=(const PluginModule&) = delete;
    PluginModule(PluginModule&&) noexcept;
    PluginModule& operator=(PluginModule&&) noexcept;

    const daux_plugin_factory*    factory() const noexcept { return factory_; }
    const daux_plugin_descriptor* descriptor() const noexcept { return descriptor_; }
    const std::string&            path() const noexcept { return path_; }

    PluginInstance create(const daux_host_callbacks* host = nullptr);

private:
    void unload() noexcept;

    std::string                   path_;
    void*                         lib_        = nullptr;
    const daux_plugin_factory*    factory_    = nullptr;
    const daux_plugin_descriptor* descriptor_ = nullptr;
};

class PluginInstance {
public:
    PluginInstance() = default;
    PluginInstance(const daux_plugin_factory* factory, daux_plugin_instance inst)
        : factory_(factory), inst_(inst) {}
    ~PluginInstance();

    PluginInstance(const PluginInstance&) = delete;
    PluginInstance& operator=(const PluginInstance&) = delete;
    PluginInstance(PluginInstance&&) noexcept;
    PluginInstance& operator=(PluginInstance&&) noexcept;

    bool valid() const noexcept { return inst_.handle != nullptr; }
    daux_plugin_handle        handle() const noexcept { return inst_.handle; }
    const daux_plugin_vtable* vtable() const noexcept { return inst_.vtable; }

    void prepare(double sample_rate, uint32_t max_block);
    void activate();
    void deactivate();
    void process(const daux_process_data& data);

    uint32_t        parameter_count();
    daux_param_info parameter_info(uint32_t index);
    double          parameter_value(daux_param_id id);
    void            set_parameter_value(daux_param_id id, double v);

    std::vector<uint8_t> get_state();
    void                 set_state(const std::vector<uint8_t>& blob);

private:
    void reset_self() noexcept;

    const daux_plugin_factory* factory_ = nullptr;
    daux_plugin_instance       inst_{nullptr, nullptr};
};

const daux_host_callbacks* default_host_callbacks() noexcept;

daux_result editor_attach_and_show(const daux_editor& ed, daux_native_window parent);

} // namespace daux

#endif /* DAUX_CORE_HPP */

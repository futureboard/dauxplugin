/*
 * daux_core.hpp - Internal C++ helper API for the DAUx host side.
 *
 * IMPORTANT: This is NOT part of the public plugin ABI. It is a C++ convenience
 * layer that the host (DAUxHost / DAUxScan) and C++ tests link against. It wraps
 * the pure C ABI declared in the daux_*.h headers with RAII and exceptions.
 * Plugins must NEVER depend on this header - they only see the C ABI.
 *
 * Everything here lives in namespace `daux`.
 */
#ifndef DAUX_CORE_HPP
#define DAUX_CORE_HPP

#include <string>
#include <stdexcept>
#include <vector>
#include <cstdint>

#include "daux_plugin.h"

namespace daux {

/* Human-readable name for a result code (defined in daux_abi.cpp). */
const char* result_to_string(daux_result r) noexcept;

/* True if the plugin factory's ABI major version matches the host's. */
bool abi_compatible(uint32_t plugin_abi_version) noexcept;

/* Format a packed version as "M.m.p". */
std::string version_to_string(uint32_t packed);

/* Thrown by the helper classes below on failure. */
class DauxError : public std::runtime_error {
public:
    explicit DauxError(const std::string& what, daux_result code = DAUX_ERR_UNKNOWN)
        : std::runtime_error(what), code_(code) {}
    daux_result code() const noexcept { return code_; }
private:
    daux_result code_;
};

/* ---------------------------------------------------------------------------
 * PluginModule - owns a loaded .dauxplug dynamic library.
 *
 * Resolves daux_plugin_entry, validates ABI, exposes the factory + descriptor,
 * and creates instances. RAII: unloads the library on destruction.
 * ------------------------------------------------------------------------- */
class PluginInstance; // fwd

class PluginModule {
public:
    /* Loads the file at `path` (.dauxplug). Throws DauxError on failure. */
    explicit PluginModule(const std::string& path);
    ~PluginModule();

    PluginModule(const PluginModule&) = delete;
    PluginModule& operator=(const PluginModule&) = delete;
    PluginModule(PluginModule&&) noexcept;
    PluginModule& operator=(PluginModule&&) noexcept;

    const daux_plugin_factory*    factory() const noexcept { return factory_; }
    const daux_plugin_descriptor* descriptor() const noexcept { return descriptor_; }
    const std::string&            path() const noexcept { return path_; }

    /* Create an instance. `host` may be null for scanning. */
    PluginInstance create(const daux_host_callbacks* host = nullptr);

private:
    void unload() noexcept;

    std::string                   path_;
    void*                         lib_      = nullptr; // HMODULE / void*
    const daux_plugin_factory*    factory_  = nullptr;
    const daux_plugin_descriptor* descriptor_ = nullptr;
};

/* ---------------------------------------------------------------------------
 * PluginInstance - owns one created instance; calls destroy() on drop.
 * ------------------------------------------------------------------------- */
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

    /* Thin checked wrappers over the most common vtable calls. */
    void prepare(double sample_rate, uint32_t max_block);
    void activate();
    void deactivate();
    void process(const daux_process_data& data);

    uint32_t        parameter_count();
    daux_param_info parameter_info(uint32_t index);
    double          parameter_value(daux_param_id id);
    void            set_parameter_value(daux_param_id id, double v);

    /* Returns the full plugin state as a byte blob. Empty if unsupported. */
    std::vector<uint8_t> get_state();
    void                 set_state(const std::vector<uint8_t>& blob);

private:
    void reset_self() noexcept;

    const daux_plugin_factory* factory_ = nullptr;
    daux_plugin_instance       inst_{nullptr, nullptr};
};

/* A ready-made host callback table that logs to stderr and ignores the rest.
 * Useful for headless processing/scanning. Returned by reference; static. */
const daux_host_callbacks* default_host_callbacks() noexcept;

} // namespace daux

#endif /* DAUX_CORE_HPP */

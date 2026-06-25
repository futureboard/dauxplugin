/*
 * daux_plugin.cpp - PluginModule / PluginInstance loader implementation.
 *
 * Host-side C++ helper (daux_core). Wraps the OS dynamic-loader and the C ABI
 * factory/vtable with RAII. Windows-first (LoadLibrary/GetProcAddress); a POSIX
 * path is provided so the code also builds on Linux/macOS.
 */
#include <DAUx/Core.hpp>

#include <utility>
#include <filesystem>
#include <fstream>
#include <iterator>

#if defined(_WIN32)
#  define WIN32_LEAN_AND_MEAN
#  include <windows.h>
#else
#  include <dlfcn.h>
#endif

namespace daux {

namespace {

void* os_load_library(const std::string& path, std::string& err) {
#if defined(_WIN32)
    /* .dauxplug is a regular PE DLL renamed; LoadLibrary loads it fine.
     *
     * Bundle support: register the module's OWN directory as a DLL search
     * directory and load with LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR so the plugin's
     * private native dependencies (e.g. an Avalonia plugin's libSkiaSharp.dll
     * shipped inside the bundle) resolve - both at load time and for later
     * dynamic P/Invoke from a managed/AOT plugin (via AddDllDirectory feeding
     * the process default search path). Use an absolute path: the search-dir
     * APIs require it. */
    std::error_code ec;
    std::filesystem::path abs = std::filesystem::absolute(path, ec);
    std::wstring wdir = abs.parent_path().wstring();

    ::SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
    if (!wdir.empty()) ::AddDllDirectory(wdir.c_str());

    HMODULE h = ::LoadLibraryExW(
        abs.c_str(), nullptr,
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS | LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR);
    if (!h) {
        err = "LoadLibrary failed (code " + std::to_string((unsigned long)::GetLastError()) + ")";
    }
    return reinterpret_cast<void*>(h);
#else
    void* h = ::dlopen(path.c_str(), RTLD_NOW | RTLD_LOCAL);
    if (!h) {
        const char* e = ::dlerror();
        err = e ? e : "dlopen failed";
    }
    return h;
#endif
}

void* os_get_symbol(void* lib, const char* name) {
#if defined(_WIN32)
    return reinterpret_cast<void*>(::GetProcAddress(reinterpret_cast<HMODULE>(lib), name));
#else
    return ::dlsym(lib, name);
#endif
}

void os_free_library(void* lib) {
    if (!lib) return;
#if defined(_WIN32)
    ::FreeLibrary(reinterpret_cast<HMODULE>(lib));
#else
    ::dlclose(lib);
#endif
}

/* Platform shared-library extension and a predicate for "is this a loadable
 * module file", so bundle resolution works on Windows/.dll, macOS/.dylib and
 * Linux/.so. */
#if defined(_WIN32)
constexpr const char* kSharedExt = ".dll";
#elif defined(__APPLE__)
constexpr const char* kSharedExt = ".dylib";
#else
constexpr const char* kSharedExt = ".so";
#endif

bool is_shared_lib(const std::filesystem::path& p) {
    const std::string e = p.extension().string();
    return e == ".dll" || e == ".so" || e == ".dylib";
}

/* Minimal extraction of <exec>...</exec> from a bundle manifest.xml. Tolerant
 * and dependency-free (no XML library): the manifest is a small, controlled
 * file. Returns the trimmed contents, or "" if absent/unreadable. */
std::string read_manifest_exec(const std::filesystem::path& manifest) {
    std::ifstream f(manifest, std::ios::binary);
    if (!f) return {};
    std::string content((std::istreambuf_iterator<char>(f)),
                        std::istreambuf_iterator<char>());
    const std::string open = "<exec>", close = "</exec>";
    size_t a = content.find(open);
    if (a == std::string::npos) return {};
    a += open.size();
    size_t b = content.find(close, a);
    if (b == std::string::npos) return {};
    std::string s = content.substr(a, b - a);
    // trim surrounding whitespace
    size_t i = s.find_first_not_of(" \t\r\n");
    size_t j = s.find_last_not_of(" \t\r\n");
    return (i == std::string::npos) ? std::string{} : s.substr(i, j - i + 1);
}

/* Resolve the actual module file to load from a .dauxplug path.
 *
 * A .dauxplug may be EITHER:
 *   - a single dynamic library file (monolithic; C++ / Rust / headless .NET), or
 *   - a BUNDLE directory "Name.dauxplug/" (VST3/macOS-bundle style) laid out as:
 *
 *       Name.dauxplug/
 *         Exec/Name.dll       <- entry module (or <exec> from manifest.xml)
 *         Library/*.dll       <- private native dependencies (Skia, ...)
 *         Resources/*         <- assets (icons, etc.)
 *         manifest.xml        <- optional metadata + entry override
 *
 * Resolution order for the entry module:
 *   1. manifest.xml <exec> (resolved under Exec/),
 *   2. Exec/Name.dll  (Name = bundle stem),
 *   3. any *.dll under Exec/,
 *   4. legacy flat layout: Name.dll / any *.dll in the bundle root.
 *
 * register_search_dirs() adds Library/ (+ Exec/ + root) to the DLL search path
 * so the private deps resolve. */
std::string resolve_module_path(const std::string& path) {
    namespace fs = std::filesystem;
    std::error_code ec;
    if (!fs::is_directory(path, ec)) return path; // plain file (single-file plugin)

    fs::path dir(path);
    std::string stem = dir.stem().string(); // "Name.dauxplug" -> "Name"
    fs::path exec_dir = dir / "Exec";

    // 1. manifest <exec> override (filename, resolved under Exec/). Tried
    //    as-is first (a platform-correct manifest), then with the platform ext
    //    swapped in (a cross-platform bundle whose manifest names e.g. .dll).
    fs::path manifest = dir / "manifest.xml";
    std::string exec_name = fs::exists(manifest, ec) ? read_manifest_exec(manifest) : std::string{};
    if (!exec_name.empty()) {
        fs::path c = exec_dir / exec_name;
        if (fs::exists(c, ec)) return c.string();
        c = exec_dir / (fs::path(exec_name).stem().string() + kSharedExt);
        if (fs::exists(c, ec)) return c.string();
    }

    // 2. convention: Exec/<stem><platform-ext>
    fs::path candidate = exec_dir / (stem + kSharedExt);
    if (fs::exists(candidate, ec)) return candidate.string();

    // 3. any shared library under Exec/
    if (fs::is_directory(exec_dir, ec)) {
        for (auto& e : fs::directory_iterator(exec_dir, ec))
            if (is_shared_lib(e.path())) return e.path().string();
    }

    // 4. legacy flat bundle
    candidate = dir / (stem + kSharedExt);
    if (fs::exists(candidate, ec)) return candidate.string();
    for (auto& e : fs::directory_iterator(dir, ec))
        if (is_shared_lib(e.path())) return e.path().string();

    return path; // let the loader produce a sensible error
}

/* Register a bundle's directories on the process DLL search path so the entry
 * module's private dependencies (in Library/) resolve - both at load time and
 * for dynamic P/Invoke made later by a managed/AOT plugin. No-op for a single
 * file or on non-Windows. */
void register_search_dirs(const std::string& path) {
#if defined(_WIN32)
    namespace fs = std::filesystem;
    std::error_code ec;
    fs::path dir = fs::absolute(path, ec);
    if (!fs::is_directory(dir, ec)) return;

    ::SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
    auto add = [](const fs::path& p) {
        std::wstring w = p.wstring();
        if (!w.empty()) ::AddDllDirectory(w.c_str());
    };
    add(dir);
    add(dir / "Library");
    add(dir / "Exec");
#else
    (void)path;
#endif
}

} // namespace

/* ===========================  PluginModule  =============================== */

PluginModule::PluginModule(const std::string& path) : path_(path) {
    std::string err;
    /* path_ keeps the bundle/file path the caller gave us (for display); we
     * load the resolved inner module and register the bundle's search dirs so
     * its private deps (Library/) resolve. */
    register_search_dirs(path);
    std::string module_path = resolve_module_path(path);
    lib_ = os_load_library(module_path, err);
    if (!lib_) {
        throw DauxError("failed to load '" + path + "': " + err, DAUX_ERR_IO);
    }

    auto entry = reinterpret_cast<daux_plugin_entry_fn>(
        os_get_symbol(lib_, DAUX_PLUGIN_ENTRY_SYMBOL));
    if (!entry) {
        unload();
        throw DauxError("'" + path + "' is not a DAUx plugin: missing entry symbol '"
                            + DAUX_PLUGIN_ENTRY_SYMBOL + "'",
                        DAUX_ERR_NOT_FOUND);
    }

    factory_ = entry(DAUX_ABI_VERSION);
    if (!factory_) {
        unload();
        throw DauxError("plugin entry returned null factory (host rejected?)", DAUX_ERR_ABI_MISMATCH);
    }
    if (!abi_compatible(factory_->abi_version)) {
        std::string fv = version_to_string(factory_->abi_version);
        unload();
        throw DauxError("ABI mismatch: plugin=" + fv + " host="
                            + version_to_string(DAUX_ABI_VERSION),
                        DAUX_ERR_ABI_MISMATCH);
    }
    if (!factory_->get_descriptor || !factory_->create || !factory_->destroy) {
        unload();
        throw DauxError("plugin factory has null required function pointers", DAUX_ERR_INVALID_STATE);
    }

    descriptor_ = factory_->get_descriptor();
    if (!descriptor_) {
        unload();
        throw DauxError("plugin returned null descriptor", DAUX_ERR_INVALID_STATE);
    }
}

PluginModule::~PluginModule() { unload(); }

PluginModule::PluginModule(PluginModule&& o) noexcept
    : path_(std::move(o.path_)), lib_(o.lib_),
      factory_(o.factory_), descriptor_(o.descriptor_) {
    o.lib_ = nullptr; o.factory_ = nullptr; o.descriptor_ = nullptr;
}

PluginModule& PluginModule::operator=(PluginModule&& o) noexcept {
    if (this != &o) {
        unload();
        path_ = std::move(o.path_);
        lib_ = o.lib_; factory_ = o.factory_; descriptor_ = o.descriptor_;
        o.lib_ = nullptr; o.factory_ = nullptr; o.descriptor_ = nullptr;
    }
    return *this;
}

void PluginModule::unload() noexcept {
    factory_ = nullptr;
    descriptor_ = nullptr;
    if (lib_) {
        os_free_library(lib_);
        lib_ = nullptr;
    }
}

PluginInstance PluginModule::create(const daux_host_callbacks* host) {
    daux_plugin_instance inst{nullptr, nullptr};
    daux_result r = factory_->create(host, &inst);
    if (r != DAUX_OK || !inst.handle || !inst.vtable) {
        throw DauxError(std::string("create() failed: ") + result_to_string(r), r);
    }
    return PluginInstance(factory_, inst);
}

/* ==========================  PluginInstance  ============================== */

PluginInstance::~PluginInstance() { reset_self(); }

PluginInstance::PluginInstance(PluginInstance&& o) noexcept
    : factory_(o.factory_), inst_(o.inst_) {
    o.factory_ = nullptr; o.inst_ = {nullptr, nullptr};
}

PluginInstance& PluginInstance::operator=(PluginInstance&& o) noexcept {
    if (this != &o) {
        reset_self();
        factory_ = o.factory_; inst_ = o.inst_;
        o.factory_ = nullptr; o.inst_ = {nullptr, nullptr};
    }
    return *this;
}

void PluginInstance::reset_self() noexcept {
    if (factory_ && inst_.handle) {
        factory_->destroy(inst_.handle);
    }
    factory_ = nullptr;
    inst_ = {nullptr, nullptr};
}

void PluginInstance::prepare(double sample_rate, uint32_t max_block) {
    daux_result r = inst_.vtable->prepare(inst_.handle, sample_rate, max_block);
    if (r != DAUX_OK) throw DauxError(std::string("prepare(): ") + result_to_string(r), r);
}

void PluginInstance::activate() {
    daux_result r = inst_.vtable->activate(inst_.handle);
    if (r != DAUX_OK) throw DauxError(std::string("activate(): ") + result_to_string(r), r);
}

void PluginInstance::deactivate() {
    daux_result r = inst_.vtable->deactivate(inst_.handle);
    if (r != DAUX_OK) throw DauxError(std::string("deactivate(): ") + result_to_string(r), r);
}

void PluginInstance::process(const daux_process_data& data) {
    daux_result r = inst_.vtable->process(inst_.handle, &data);
    if (r != DAUX_OK) throw DauxError(std::string("process(): ") + result_to_string(r), r);
}

uint32_t PluginInstance::parameter_count() {
    return inst_.vtable->get_parameter_count(inst_.handle);
}

daux_param_info PluginInstance::parameter_info(uint32_t index) {
    daux_param_info info{};
    daux_result r = inst_.vtable->get_parameter_info(inst_.handle, index, &info);
    if (r != DAUX_OK) throw DauxError(std::string("get_parameter_info(): ") + result_to_string(r), r);
    return info;
}

double PluginInstance::parameter_value(daux_param_id id) {
    double v = 0.0;
    daux_result r = inst_.vtable->get_parameter_value(inst_.handle, id, &v);
    if (r != DAUX_OK) throw DauxError(std::string("get_parameter_value(): ") + result_to_string(r), r);
    return v;
}

void PluginInstance::set_parameter_value(daux_param_id id, double v) {
    daux_result r = inst_.vtable->set_parameter_value(inst_.handle, id, v);
    if (r != DAUX_OK) throw DauxError(std::string("set_parameter_value(): ") + result_to_string(r), r);
}

std::vector<uint8_t> PluginInstance::get_state() {
    if (!inst_.vtable->get_state) return {};
    uint32_t needed = 0;
    /* First call: size query. */
    daux_result r = inst_.vtable->get_state(inst_.handle, nullptr, 0, &needed);
    if (r != DAUX_OK && r != DAUX_ERR_BUFFER_TOO_SMALL)
        throw DauxError(std::string("get_state(size): ") + result_to_string(r), r);
    if (needed == 0) return {};
    std::vector<uint8_t> buf(needed);
    r = inst_.vtable->get_state(inst_.handle, buf.data(), needed, &needed);
    if (r != DAUX_OK) throw DauxError(std::string("get_state(fill): ") + result_to_string(r), r);
    buf.resize(needed);
    return buf;
}

void PluginInstance::set_state(const std::vector<uint8_t>& blob) {
    if (!inst_.vtable->set_state)
        throw DauxError("plugin does not support set_state", DAUX_ERR_NOT_SUPPORTED);
    daux_result r = inst_.vtable->set_state(inst_.handle, blob.data(),
                                            (uint32_t)blob.size());
    if (r != DAUX_OK) throw DauxError(std::string("set_state(): ") + result_to_string(r), r);
}

} // namespace daux

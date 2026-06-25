/*
 * host.hpp - Internal declarations shared across DAUxHost translation units.
 *
 * DAUxHost.exe loads .dauxplug modules out-of-process from the DAW, hosts their
 * editors, exposes IPC to Futureboard, and can scan plugin metadata. This first
 * cut implements the headless/scan paths fully and leaves editor + IPC as
 * clearly-marked skeletons.
 */
#ifndef DAUXHOST_HOST_HPP
#define DAUXHOST_HOST_HPP

#include <string>
#include <vector>
#include <cstdint>

#include "daux_core.hpp"

namespace dauxhost {

/* Run modes (parsed from --mode=...). See dauxhost-protocol.md. */
enum class Mode {
    Scan,       // enumerate/dump metadata, then exit
    Plugin,     // load + process audio, driven over IPC (skeleton)
    Editor,     // load + host the plugin editor window (skeleton)
    Headless    // load + run a self-test process block, then exit
};

struct Options {
    Mode        mode = Mode::Headless;
    std::string plugin_path;     // --plugin=PATH
    std::string scan_dir;        // --scan-dir=DIR  (Scan mode)
    std::string ipc_endpoint;    // --ipc=NAME      (Plugin/Editor modes)
    uint32_t    sample_rate = 48000;
    uint32_t    block_size  = 512;
    uint32_t    hold_seconds = 0;     // --hold=N (editor mode): auto-close after N s
    bool        json        = false;  // --json metadata output
    bool        show_help   = false;
};

bool parse_options(int argc, char** argv, Options& out, std::string& error);
void print_usage(const char* argv0);

/* --- plugin_loader.cpp --- */
/* Print a human or JSON description of a loaded module's descriptor + params. */
void dump_metadata(const daux::PluginModule& mod, daux::PluginInstance& inst, bool json);

/* --- plugin_editor.cpp --- */
/* Create the plugin's editor, report its preferred size, then destroy it.
 * Returns false if the plugin has no editor or creation fails. Does not yet
 * attach to a window (no window system wired up in this milestone). */
bool probe_editor(daux::PluginInstance& inst, const daux_host_callbacks* host);

/* --- host_runtime.cpp --- */
int run_headless(const Options& opt);   // load, prepare, process one block, report
int run_plugin(const Options& opt);     // skeleton: load + IPC loop
int run_editor(const Options& opt);     // skeleton: load + host editor

/* --- plugin_scan.cpp --- */
struct ScanResult {
    std::string path;
    bool        ok = false;
    std::string id;
    std::string name;
    std::string vendor;
    std::string version;
    std::string error;
};
std::vector<ScanResult> scan_directory(const std::string& dir);
int run_scan(const Options& opt);

/* --- ipc_server.cpp --- */
/* Minimal transport abstraction. The reference build provides a no-op/stdio
 * implementation; a real named-pipe/shared-memory transport is a TODO. */
class IpcServer {
public:
    explicit IpcServer(std::string endpoint);
    ~IpcServer();
    bool start();                 // returns false if transport unavailable
    void stop();
    bool send(const std::string& msg);
    // Blocking receive of one line/frame; false on shutdown. TODO: real impl.
    bool receive(std::string& out);
private:
    std::string endpoint_;
    bool        running_ = false;
};

} // namespace dauxhost

#endif /* DAUXHOST_HOST_HPP */

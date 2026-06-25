/*
 * main.cpp - DAUxHost.exe entry point and CLI parsing.
 *
 * Dispatches to one of the run modes:
 *   --mode=scan      enumerate plugins in a directory (or one plugin) and dump
 *                    metadata (text or --json).
 *   --mode=headless  load one plugin, prepare, process a single passthrough
 *                    block, and report success. (Default.)
 *   --mode=plugin    load + serve over IPC (skeleton).
 *   --mode=editor    load + host the editor window (skeleton).
 */
#include "host.hpp"

#include <cstdio>
#include <cstring>
#include <string>

namespace dauxhost {

void print_usage(const char* argv0) {
    std::printf(
        "DAUxHost - DAUx out-of-process plugin host\n"
        "\n"
        "Usage: %s [--mode=MODE] [options]\n"
        "\n"
        "Modes:\n"
        "  --mode=scan      Scan plugin(s) and dump metadata, then exit.\n"
        "  --mode=headless  Load + process one block, then exit (default).\n"
        "  --mode=plugin    Load + serve over IPC (skeleton).\n"
        "  --mode=editor    Load + host the editor window (skeleton).\n"
        "\n"
        "Options:\n"
        "  --plugin=PATH    Path to a .dauxplug to load.\n"
        "  --scan-dir=DIR   Directory to scan (scan mode).\n"
        "  --ipc=NAME       IPC endpoint name (plugin/editor modes).\n"
        "  --sample-rate=N  Sample rate for processing (default 48000).\n"
        "  --block-size=N   Max block size (default 512).\n"
        "  --hold=N         Editor mode: auto-close after N seconds (0 = wait for Enter).\n"
        "  --json           Emit metadata as JSON.\n"
        "  --help           Show this help.\n",
        argv0);
}

static bool starts_with(const char* s, const char* p, const char** rest) {
    size_t n = std::strlen(p);
    if (std::strncmp(s, p, n) == 0) { *rest = s + n; return true; }
    return false;
}

bool parse_options(int argc, char** argv, Options& out, std::string& error) {
    for (int i = 1; i < argc; ++i) {
        const char* a = argv[i];
        const char* rest = nullptr;
        if (std::strcmp(a, "--help") == 0 || std::strcmp(a, "-h") == 0) {
            out.show_help = true;
        } else if (starts_with(a, "--mode=", &rest)) {
            if (std::strcmp(rest, "scan") == 0)          out.mode = Mode::Scan;
            else if (std::strcmp(rest, "headless") == 0) out.mode = Mode::Headless;
            else if (std::strcmp(rest, "plugin") == 0)   out.mode = Mode::Plugin;
            else if (std::strcmp(rest, "editor") == 0)   out.mode = Mode::Editor;
            else { error = std::string("unknown mode: ") + rest; return false; }
        } else if (starts_with(a, "--plugin=", &rest)) {
            out.plugin_path = rest;
        } else if (starts_with(a, "--scan-dir=", &rest)) {
            out.scan_dir = rest;
        } else if (starts_with(a, "--ipc=", &rest)) {
            out.ipc_endpoint = rest;
        } else if (starts_with(a, "--sample-rate=", &rest)) {
            out.sample_rate = (uint32_t)std::strtoul(rest, nullptr, 10);
        } else if (starts_with(a, "--block-size=", &rest)) {
            out.block_size = (uint32_t)std::strtoul(rest, nullptr, 10);
        } else if (starts_with(a, "--hold=", &rest)) {
            out.hold_seconds = (uint32_t)std::strtoul(rest, nullptr, 10);
        } else if (std::strcmp(a, "--json") == 0) {
            out.json = true;
        } else {
            error = std::string("unknown argument: ") + a;
            return false;
        }
    }
    return true;
}

} // namespace dauxhost

int main(int argc, char** argv) {
    using namespace dauxhost;

    Options opt;
    std::string err;
    if (!parse_options(argc, argv, opt, err)) {
        std::fprintf(stderr, "error: %s\n\n", err.c_str());
        print_usage(argv[0]);
        return 2;
    }
    if (opt.show_help) {
        print_usage(argv[0]);
        return 0;
    }

    switch (opt.mode) {
        case Mode::Scan:     return run_scan(opt);
        case Mode::Headless: return run_headless(opt);
        case Mode::Plugin:   return run_plugin(opt);
        case Mode::Editor:   return run_editor(opt);
    }
    return 0;
}

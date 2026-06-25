/*
 * DAUxScan - standalone plugin scanner utility.
 *
 * A thin CLI over dauxhost::scan_directory / single-plugin metadata dump. Kept
 * as a separate executable so a DAW can shell out to a tiny, fast scanner
 * without pulling in the full host's editor/IPC machinery.
 *
 * Usage:
 *   DAUxScan <dir>                 scan a directory recursively (text)
 *   DAUxScan --json <dir>          scan, emit JSON
 *   DAUxScan --plugin <file>       dump one plugin's full metadata
 */
#include "host.hpp"

#include <cstdio>
#include <cstring>
#include <string>

int main(int argc, char** argv) {
    using namespace dauxhost;

    Options opt;
    opt.mode = Mode::Scan;

    for (int i = 1; i < argc; ++i) {
        const char* a = argv[i];
        if (std::strcmp(a, "--json") == 0) {
            opt.json = true;
        } else if (std::strcmp(a, "--plugin") == 0 && i + 1 < argc) {
            opt.plugin_path = argv[++i];
        } else if (std::strcmp(a, "--help") == 0 || std::strcmp(a, "-h") == 0) {
            std::printf("DAUxScan - DAUx plugin scanner\n"
                        "Usage:\n"
                        "  DAUxScan [--json] <dir>        scan a directory\n"
                        "  DAUxScan [--json] --plugin <f> dump one plugin\n");
            return 0;
        } else if (a[0] != '-') {
            opt.scan_dir = a;
        } else {
            std::fprintf(stderr, "unknown argument: %s\n", a);
            return 2;
        }
    }

    if (opt.plugin_path.empty() && opt.scan_dir.empty()) {
        std::fprintf(stderr, "error: provide a directory or --plugin <file>\n");
        return 2;
    }
    return run_scan(opt);
}

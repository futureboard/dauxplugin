/*
 * plugin_scan.cpp - Directory scanner for .dauxplug modules.
 *
 * Walks a directory (recursively) for *.dauxplug files and attempts to read
 * each one's descriptor without keeping instances around. Failures are captured
 * per-file with a readable error rather than aborting the whole scan. This is
 * the minimal scanner the first milestone asks for; DAUxScan reuses it.
 */
#include "host.hpp"

#include <cstdio>
#include <filesystem>

namespace fs = std::filesystem;

namespace dauxhost {

std::vector<ScanResult> scan_directory(const std::string& dir) {
    std::vector<ScanResult> results;
    std::error_code ec;
    if (!fs::exists(dir, ec) || !fs::is_directory(dir, ec)) {
        ScanResult r;
        r.path = dir;
        r.ok = false;
        r.error = "not a directory";
        results.push_back(std::move(r));
        return results;
    }

    for (auto it = fs::recursive_directory_iterator(
             dir, fs::directory_options::skip_permission_denied, ec);
         it != fs::recursive_directory_iterator(); it.increment(ec)) {
        if (ec) break;
        const fs::directory_entry& e = *it;

        const bool is_bundle = e.is_directory(ec) && e.path().extension() == ".dauxplug";
        const bool is_file   = e.is_regular_file(ec) && e.path().extension() == ".dauxplug";
        if (is_bundle) {
            // A bundle is a plugin; don't recurse into its private contents.
            it.disable_recursion_pending();
        } else if (!is_file) {
            continue;
        }

        ScanResult r;
        r.path = e.path().string();
        try {
            daux::PluginModule mod(r.path);
            const daux_plugin_descriptor* d = mod.descriptor();
            r.ok = true;
            r.id = d->id;
            r.name = d->name;
            r.vendor = d->vendor;
            r.version = daux::version_to_string(d->version);
        } catch (const std::exception& ex) {
            r.ok = false;
            r.error = ex.what();
        }
        results.push_back(std::move(r));
    }
    return results;
}

int run_scan(const Options& opt) {
    /* Single-plugin scan if --plugin is given; otherwise scan --scan-dir. */
    if (!opt.plugin_path.empty()) {
        try {
            daux::PluginModule mod(opt.plugin_path);
            daux::PluginInstance inst = mod.create(daux::default_host_callbacks());
            dump_metadata(mod, inst, opt.json);
            return 0;
        } catch (const std::exception& e) {
            std::fprintf(stderr, "scan failed: %s\n", e.what());
            return 1;
        }
    }

    if (opt.scan_dir.empty()) {
        std::fprintf(stderr, "error: scan mode needs --scan-dir=DIR or --plugin=PATH\n");
        return 2;
    }

    auto results = scan_directory(opt.scan_dir);
    int failures = 0;

    if (opt.json) {
        std::printf("[\n");
        for (size_t i = 0; i < results.size(); ++i) {
            const auto& r = results[i];
            if (!r.ok) ++failures;
            std::printf("  {\"path\": \"%s\", \"ok\": %s, \"id\": \"%s\", "
                        "\"name\": \"%s\", \"version\": \"%s\", \"error\": \"%s\"}%s\n",
                        r.path.c_str(), r.ok ? "true" : "false",
                        r.id.c_str(), r.name.c_str(), r.version.c_str(),
                        r.error.c_str(), (i + 1 < results.size()) ? "," : "");
        }
        std::printf("]\n");
    } else {
        for (const auto& r : results) {
            if (r.ok) {
                std::printf("[OK]   %s  (%s %s)\n", r.name.c_str(), r.id.c_str(), r.version.c_str());
                std::printf("       %s\n", r.path.c_str());
            } else {
                ++failures;
                std::printf("[FAIL] %s\n       %s\n", r.path.c_str(), r.error.c_str());
            }
        }
        std::printf("\nscanned %zu file(s), %d failure(s)\n", results.size(), failures);
    }
    return failures == 0 ? 0 : 1;
}

} // namespace dauxhost

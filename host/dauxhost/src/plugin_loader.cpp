/*
 * plugin_loader.cpp - Metadata dumping for a loaded plugin module.
 *
 * The heavy lifting (LoadLibrary, ABI validation, RAII) lives in daux_core
 * (PluginModule). This file presents that information to the user as text or
 * JSON. Keeping the loader abstraction here means the rest of the host always
 * talks in terms of ".dauxplug" regardless of the underlying OS module type.
 */
#include "host.hpp"

#include <cstdio>
#include <string>

namespace dauxhost {

namespace {

const char* category_name(daux_plugin_category c) {
    switch (c) {
        case DAUX_CATEGORY_EFFECT:     return "effect";
        case DAUX_CATEGORY_INSTRUMENT: return "instrument";
        case DAUX_CATEGORY_MIDI_FX:    return "midi-fx";
        case DAUX_CATEGORY_ANALYZER:   return "analyzer";
        default:                       return "unknown";
    }
}

std::string json_escape(const char* s) {
    std::string o;
    for (const char* p = s; p && *p; ++p) {
        char c = *p;
        if (c == '"' || c == '\\') { o.push_back('\\'); o.push_back(c); }
        else if (c == '\n') o += "\\n";
        else o.push_back(c);
    }
    return o;
}

} // namespace

void dump_metadata(const daux::PluginModule& mod, daux::PluginInstance& inst, bool json) {
    const daux_plugin_descriptor* d = mod.descriptor();
    const uint32_t caps = (uint32_t)d->capabilities;
    const uint32_t pcount = inst.valid() ? inst.parameter_count() : 0;

    if (json) {
        std::printf("{\n");
        std::printf("  \"path\": \"%s\",\n", json_escape(mod.path().c_str()).c_str());
        std::printf("  \"id\": \"%s\",\n", json_escape(d->id).c_str());
        std::printf("  \"name\": \"%s\",\n", json_escape(d->name).c_str());
        std::printf("  \"vendor\": \"%s\",\n", json_escape(d->vendor).c_str());
        std::printf("  \"version\": \"%s\",\n", daux::version_to_string(d->version).c_str());
        std::printf("  \"category\": \"%s\",\n", category_name(d->category));
        std::printf("  \"audio_input_buses\": %u,\n", d->audio_input_buses);
        std::printf("  \"audio_output_buses\": %u,\n", d->audio_output_buses);
        std::printf("  \"capabilities\": {\n");
        std::printf("    \"editor\": %s,\n",      (caps & DAUX_CAP_EDITOR) ? "true" : "false");
        std::printf("    \"midi_input\": %s,\n",  (caps & DAUX_CAP_MIDI_INPUT) ? "true" : "false");
        std::printf("    \"midi_output\": %s,\n", (caps & DAUX_CAP_MIDI_OUTPUT) ? "true" : "false");
        std::printf("    \"state\": %s,\n",       (caps & DAUX_CAP_STATE) ? "true" : "false");
        std::printf("    \"presets\": %s\n",      (caps & DAUX_CAP_PRESETS) ? "true" : "false");
        std::printf("  },\n");
        std::printf("  \"parameters\": [");
        for (uint32_t i = 0; i < pcount; ++i) {
            daux_param_info info = inst.parameter_info(i);
            std::printf("%s\n    {\"id\": %u, \"name\": \"%s\", \"unit\": \"%s\", "
                        "\"min\": %g, \"max\": %g, \"default\": %g}",
                        i ? "," : "", info.id, json_escape(info.name).c_str(),
                        json_escape(info.unit).c_str(),
                        info.min_value, info.max_value, info.default_value);
        }
        std::printf("%s]\n}\n", pcount ? "\n  " : "");
        return;
    }

    std::printf("Plugin: %s\n", d->name);
    std::printf("  path     : %s\n", mod.path().c_str());
    std::printf("  id       : %s\n", d->id);
    std::printf("  vendor   : %s\n", d->vendor);
    std::printf("  version  : %s\n", daux::version_to_string(d->version).c_str());
    std::printf("  category : %s\n", category_name(d->category));
    std::printf("  buses    : %u in / %u out\n", d->audio_input_buses, d->audio_output_buses);
    std::printf("  caps     : %s%s%s%s%s\n",
                (caps & DAUX_CAP_EDITOR) ? "editor " : "",
                (caps & DAUX_CAP_MIDI_INPUT) ? "midi-in " : "",
                (caps & DAUX_CAP_MIDI_OUTPUT) ? "midi-out " : "",
                (caps & DAUX_CAP_STATE) ? "state " : "",
                (caps & DAUX_CAP_PRESETS) ? "presets " : "");
    std::printf("  params   : %u\n", pcount);
    for (uint32_t i = 0; i < pcount; ++i) {
        daux_param_info info = inst.parameter_info(i);
        std::printf("    [%u] %-16s %g..%g (def %g) %s\n", info.id, info.name,
                    info.min_value, info.max_value, info.default_value, info.unit);
    }
}

} // namespace dauxhost

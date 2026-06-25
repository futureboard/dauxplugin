/*
 * host_runtime.cpp - The run modes: headless self-test, plugin (IPC), editor.
 *
 * run_headless is the fully-implemented path required by the first milestone:
 * load a plugin, dump metadata, create an instance, prepare(), feed it one
 * block of audio, and verify process() runs. run_plugin / run_editor are
 * skeletons that wire up the loader + transport but mark the loop as TODO.
 */
#include "host.hpp"

#include <cstdio>
#include <vector>

#if defined(_WIN32)
#  define WIN32_LEAN_AND_MEAN
#  include <windows.h>
#endif

namespace dauxhost {

namespace {

/* Build a temporary planar audio buffer set for the descriptor's bus layout. */
struct ScratchAudio {
    std::vector<std::vector<float>>     storage;   // one vector per channel
    std::vector<std::vector<float*>>    bus_ptrs;  // per-bus channel ptr arrays
    std::vector<daux_audio_bus_buffer>  buses;

    void build(uint32_t bus_count, uint32_t channels, uint32_t frames, float fill) {
        buses.resize(bus_count);
        bus_ptrs.resize(bus_count);
        for (uint32_t b = 0; b < bus_count; ++b) {
            bus_ptrs[b].clear();
            for (uint32_t c = 0; c < channels; ++c) {
                storage.emplace_back(frames, fill);
                bus_ptrs[b].push_back(storage.back().data());
            }
            buses[b].channel_count = channels;
            buses[b].channels = bus_ptrs[b].data();
            buses[b].channels64 = nullptr;
        }
    }
};

/* Host callbacks for editor mode: print parameter edits so the user can see
 * the editor is wired to the host (when they move the Gain slider). */
void DAUX_CALL editor_param_changed(daux_host_context, daux_param_id id, double normalized) {
    std::printf("  [host] param %u changed -> %.3f (normalized)\n", id, normalized);
    std::fflush(stdout);
}
void DAUX_CALL editor_log(daux_host_context, int32_t level, const char* msg) {
    std::printf("  [plugin log %d] %s\n", level, msg ? msg : "");
    std::fflush(stdout);
}

const daux_host_callbacks* editor_host_callbacks() {
    static daux_host_callbacks cb = [] {
        daux_host_callbacks c{};
        c.struct_size = (uint32_t)sizeof(daux_host_callbacks);
        c.abi_version = DAUX_ABI_VERSION;
        c.param_changed = editor_param_changed;
        c.log = editor_log;
        return c;
    }();
    return &cb;
}

} // namespace

int run_headless(const Options& opt) {
    if (opt.plugin_path.empty()) {
        std::fprintf(stderr, "error: --plugin=PATH is required for headless mode\n");
        return 2;
    }
    try {
        daux::PluginModule mod(opt.plugin_path);
        daux::PluginInstance inst = mod.create(daux::default_host_callbacks());

        dump_metadata(mod, inst, opt.json);

        const daux_plugin_descriptor* d = mod.descriptor();
        const uint32_t channels = d->default_channels_per_bus ? d->default_channels_per_bus : 2;
        const uint32_t frames = opt.block_size;

        inst.prepare((double)opt.sample_rate, opt.block_size);
        inst.activate();

        /* Set up one block: inputs filled with a known value, outputs zeroed. */
        ScratchAudio in, out;
        in.build(d->audio_input_buses, channels, frames, 0.5f);
        out.build(d->audio_output_buses, channels, frames, 0.0f);

        daux_process_context ctx{};
        ctx.sample_rate = (double)opt.sample_rate;
        ctx.block_size = frames;
        ctx.sample_format = DAUX_SAMPLE_F32;
        ctx.tempo_bpm = 120.0;
        ctx.time_sig_numerator = 4;
        ctx.time_sig_denominator = 4;
        ctx.is_playing = DAUX_TRUE;

        daux_process_data pd{};
        pd.context = &ctx;
        pd.input_bus_count = (uint32_t)in.buses.size();
        pd.inputs = in.buses.empty() ? nullptr : in.buses.data();
        pd.output_bus_count = (uint32_t)out.buses.size();
        pd.outputs = out.buses.empty() ? nullptr : out.buses.data();
        pd.num_frames = frames;

        inst.process(pd);
        inst.deactivate();

        /* Sanity report: show the first output sample so the user can see the
         * plugin actually touched the buffer. */
        float first = (out.buses.size() && out.buses[0].channel_count)
                          ? out.buses[0].channels[0][0] : 0.0f;
        std::printf("\nheadless self-test: processed %u frames x %u ch; out[0][0]=%.4f\n",
                    frames, channels, first);
        return 0;
    } catch (const daux::DauxError& e) {
        std::fprintf(stderr, "DAUx error: %s\n", e.what());
        return 1;
    } catch (const std::exception& e) {
        std::fprintf(stderr, "error: %s\n", e.what());
        return 1;
    }
}

int run_plugin(const Options& opt) {
    if (opt.plugin_path.empty()) {
        std::fprintf(stderr, "error: --plugin=PATH is required for plugin mode\n");
        return 2;
    }
    try {
        daux::PluginModule mod(opt.plugin_path);
        daux::PluginInstance inst = mod.create(daux::default_host_callbacks());
        inst.prepare((double)opt.sample_rate, opt.block_size);

        IpcServer ipc(opt.ipc_endpoint);
        if (!ipc.start()) {
            std::fprintf(stderr, "warning: IPC transport unavailable (skeleton). "
                                 "Plugin loaded and prepared; idling.\n");
        }
        /* TODO: real IPC loop -
         *   - receive process/param/state commands from Futureboard,
         *   - shuttle audio over shared memory,
         *   - reply with status. See dauxhost-protocol.md. */
        std::printf("plugin mode: '%s' loaded and prepared (IPC loop is a TODO).\n",
                    mod.descriptor()->name);
        ipc.stop();
        return 0;
    } catch (const std::exception& e) {
        std::fprintf(stderr, "error: %s\n", e.what());
        return 1;
    }
}

int run_editor(const Options& opt) {
    if (opt.plugin_path.empty()) {
        std::fprintf(stderr, "error: --plugin=PATH is required for editor mode\n");
        return 2;
    }
    try {
        daux::PluginModule mod(opt.plugin_path);
        if (!(mod.descriptor()->capabilities & DAUX_CAP_EDITOR)) {
            std::fprintf(stderr, "error: plugin '%s' has no editor.\n",
                         mod.descriptor()->name);
            return 1;
        }

        const daux_host_callbacks* host = editor_host_callbacks();
        daux::PluginInstance inst = mod.create(host);
        const daux_plugin_vtable* v = inst.vtable();

        daux_editor ed{};
        daux_result r = v->create_editor(inst.handle(), host, &ed);
        if (r != DAUX_OK || !ed.handle || !ed.vtable) {
            std::fprintf(stderr, "create_editor failed: %s\n", daux::result_to_string(r));
            return 1;
        }

        daux_size size{0, 0};
        if (ed.vtable->get_preferred_size)
            ed.vtable->get_preferred_size(ed.handle, &size);

        /* Open the editor as a standalone window owned by this host process.
         * (Embedding into a host-created frame / cross-process reparenting is
         * the remaining TODO; here we genuinely show and run the editor.) */
        if (ed.vtable->attach) ed.vtable->attach(ed.handle, nullptr);
        if (ed.vtable->show)   ed.vtable->show(ed.handle);

        std::printf("Editor '%s' opened (%ux%u).\n",
                    mod.descriptor()->name, size.width, size.height);
        std::fflush(stdout);

#if defined(_WIN32)
        if (opt.hold_seconds > 0) {
            std::printf("Holding for %u s (move the Gain slider to see host updates)...\n",
                        opt.hold_seconds);
            std::fflush(stdout);
            const uint32_t ticks = opt.hold_seconds * 30;
            for (uint32_t i = 0; i < ticks; ++i) {
                if (ed.vtable->idle) ed.vtable->idle(ed.handle);
                ::Sleep(33);
            }
        } else {
            std::printf("Press Enter here to close the editor.\n");
            std::fflush(stdout);
            std::getchar();
        }
#else
        std::printf("Press Enter here to close the editor.\n");
        std::getchar();
#endif

        if (ed.vtable->hide) ed.vtable->hide(ed.handle);
        if (v->destroy_editor) v->destroy_editor(inst.handle(), ed.handle);
        std::printf("Editor closed.\n");
        return 0;
    } catch (const std::exception& e) {
        std::fprintf(stderr, "error: %s\n", e.what());
        return 1;
    }
}

} // namespace dauxhost

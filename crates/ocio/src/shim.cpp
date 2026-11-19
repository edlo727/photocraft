// A small C API over OpenColorIO for photocraft-ocio (src/lib.rs). Only what PhotoCraft
// needs: load a config, list its colour spaces, displays, views and looks, and convert float
// RGBA between two colour spaces or through a display/view (the viewer) on the CPU. Every OCIO exception is caught here and returned as text, never thrown across the
// FFI boundary.

#include <OpenColorIO/OpenColorIO.h>

#include <cstring>
#include <exception>
#include <string>
#include <vector>

namespace OCIO = OCIO_NAMESPACE;

struct pc_ocio_config {
    OCIO::ConstConfigRcPtr config;
    std::string name;
    // Active colour spaces, in config order, with their families (owned here so the C strings
    // handed out stay valid as long as the config).
    std::vector<std::string> names;
    std::vector<std::string> families;
    // Displays in config order, each with its views; the default display; the looks.
    std::vector<std::string> displays;
    std::vector<std::vector<std::string>> views;
    std::string default_display;
    std::vector<std::string> looks;
};

struct pc_ocio_proc {
    OCIO::ConstCPUProcessorRcPtr cpu;
    bool noop;
};

static void set_err(char* err, size_t len, const char* msg) {
    if (err && len) {
        std::strncpy(err, msg, len - 1);
        err[len - 1] = '\0';
    }
}

static pc_ocio_config* wrap(OCIO::ConstConfigRcPtr cfg) {
    auto* out = new pc_ocio_config();
    out->config = cfg;
    const char* name = cfg->getName();
    out->name = name ? name : "";
    const int n = cfg->getNumColorSpaces(OCIO::SEARCH_REFERENCE_SPACE_ALL, OCIO::COLORSPACE_ACTIVE);
    for (int i = 0; i < n; ++i) {
        const char* cs = cfg->getColorSpaceNameByIndex(OCIO::SEARCH_REFERENCE_SPACE_ALL, OCIO::COLORSPACE_ACTIVE, i);
        if (!cs) continue;
        auto space = cfg->getColorSpace(cs);
        const char* family = space ? space->getFamily() : nullptr;
        out->names.emplace_back(cs);
        out->families.emplace_back(family ? family : "");
    }
    const int nd = cfg->getNumDisplays();
    for (int d = 0; d < nd; ++d) {
        const char* display = cfg->getDisplay(d);
        if (!display) continue;
        std::vector<std::string> vs;
        const int nv = cfg->getNumViews(display);
        for (int v = 0; v < nv; ++v) {
            const char* view = cfg->getView(display, v);
            if (view) vs.emplace_back(view);
        }
        out->displays.emplace_back(display);
        out->views.emplace_back(std::move(vs));
    }
    const char* def = cfg->getDefaultDisplay();
    out->default_display = def ? def : "";
    const int nl = cfg->getNumLooks();
    for (int i = 0; i < nl; ++i) {
        const char* look = cfg->getLookNameByIndex(i);
        if (look) out->looks.emplace_back(look);
    }
    return out;
}

extern "C" {

const char* pc_ocio_version() { return OCIO::GetVersion(); }

// A config from a file path or an `ocio://` built-in URI.
pc_ocio_config* pc_ocio_config_from_uri(const char* uri, char* err, size_t err_len) {
    try {
        return wrap(OCIO::Config::CreateFromFile(uri));
    } catch (const std::exception& e) {
        set_err(err, err_len, e.what());
    } catch (...) {
        set_err(err, err_len, "unknown OpenColorIO error");
    }
    return nullptr;
}

// The config `$OCIO` names (OCIO's own rules, as in Nuke).
pc_ocio_config* pc_ocio_config_from_env(char* err, size_t err_len) {
    try {
        return wrap(OCIO::Config::CreateFromEnv());
    } catch (const std::exception& e) {
        set_err(err, err_len, e.what());
    } catch (...) {
        set_err(err, err_len, "unknown OpenColorIO error");
    }
    return nullptr;
}

void pc_ocio_config_free(pc_ocio_config* c) { delete c; }

const char* pc_ocio_config_name(const pc_ocio_config* c) { return c->name.c_str(); }

int pc_ocio_colorspace_count(const pc_ocio_config* c) { return static_cast<int>(c->names.size()); }

const char* pc_ocio_colorspace_name(const pc_ocio_config* c, int i) {
    return (i >= 0 && i < static_cast<int>(c->names.size())) ? c->names[i].c_str() : nullptr;
}

const char* pc_ocio_colorspace_family(const pc_ocio_config* c, int i) {
    return (i >= 0 && i < static_cast<int>(c->families.size())) ? c->families[i].c_str() : nullptr;
}

// The colour space a role (scene_linear, compositing_log, …) names, or null.
const char* pc_ocio_role(const pc_ocio_config* c, const char* role) {
    try {
        if (!c->config->hasRole(role)) return nullptr;
        auto cs = c->config->getColorSpace(role);
        return cs ? cs->getName() : nullptr;
    } catch (...) {
        return nullptr;
    }
}

// A CPU processor from colour space `src` to `dst` (names, aliases or roles), float in and out.
pc_ocio_proc* pc_ocio_processor(const pc_ocio_config* c, const char* src, const char* dst, char* err, size_t err_len) {
    try {
        auto proc = c->config->getProcessor(src, dst);
        auto* out = new pc_ocio_proc();
        out->cpu = proc->getDefaultCPUProcessor();
        out->noop = proc->isNoOp();
        return out;
    } catch (const std::exception& e) {
        set_err(err, err_len, e.what());
    } catch (...) {
        set_err(err, err_len, "unknown OpenColorIO error");
    }
    return nullptr;
}

// Displays, their views and the looks, for the viewer's menus.
int pc_ocio_display_count(const pc_ocio_config* c) { return static_cast<int>(c->displays.size()); }

const char* pc_ocio_display_name(const pc_ocio_config* c, int d) {
    return (d >= 0 && d < static_cast<int>(c->displays.size())) ? c->displays[d].c_str() : nullptr;
}

int pc_ocio_view_count(const pc_ocio_config* c, int d) {
    return (d >= 0 && d < static_cast<int>(c->views.size())) ? static_cast<int>(c->views[d].size()) : 0;
}

const char* pc_ocio_view_name(const pc_ocio_config* c, int d, int v) {
    if (d < 0 || d >= static_cast<int>(c->views.size())) return nullptr;
    const auto& vs = c->views[d];
    return (v >= 0 && v < static_cast<int>(vs.size())) ? vs[v].c_str() : nullptr;
}

const char* pc_ocio_default_display(const pc_ocio_config* c) { return c->default_display.c_str(); }

int pc_ocio_look_count(const pc_ocio_config* c) { return static_cast<int>(c->looks.size()); }

const char* pc_ocio_look_name(const pc_ocio_config* c, int i) {
    return (i >= 0 && i < static_cast<int>(c->looks.size())) ? c->looks[i].c_str() : nullptr;
}

// The viewer: colour space `src` through `display` / `view` (Nuke's viewer process), with
// `look` (empty: the view's own looks) applied as an override, float in and out.
pc_ocio_proc* pc_ocio_display_view_processor(const pc_ocio_config* c, const char* src, const char* display, const char* view, const char* look, char* err,
                                             size_t err_len) {
    try {
        auto dvt = OCIO::DisplayViewTransform::Create();
        dvt->setSrc(src);
        dvt->setDisplay(display);
        dvt->setView(view);
        OCIO::ConstProcessorRcPtr proc;
        if (look && *look) {
            auto pipeline = OCIO::LegacyViewingPipeline::Create();
            pipeline->setDisplayViewTransform(dvt);
            pipeline->setLooksOverrideEnabled(true);
            pipeline->setLooksOverride(look);
            proc = pipeline->getProcessor(c->config, c->config->getCurrentContext());
        } else {
            proc = c->config->getProcessor(dvt);
        }
        auto* out = new pc_ocio_proc();
        out->cpu = proc->getDefaultCPUProcessor();
        out->noop = proc->isNoOp();
        return out;
    } catch (const std::exception& e) {
        set_err(err, err_len, e.what());
    } catch (...) {
        set_err(err, err_len, "unknown OpenColorIO error");
    }
    return nullptr;
}

// The inverse of the viewer process: display code values of `display` / `view` back to colour
// space `dst` (scene-linear), so display-referred pixels (an sRGB image) come back out of the same
// view as they went in, whatever the config's default view transform.
pc_ocio_proc* pc_ocio_inverse_view_processor(const pc_ocio_config* c, const char* dst, const char* display, const char* view, char* err, size_t err_len) {
    try {
        auto dvt = OCIO::DisplayViewTransform::Create();
        dvt->setSrc(dst);
        dvt->setDisplay(display);
        dvt->setView(view);
        auto proc = c->config->getProcessor(dvt, OCIO::TRANSFORM_DIR_INVERSE);
        auto* out = new pc_ocio_proc();
        out->cpu = proc->getDefaultCPUProcessor();
        out->noop = proc->isNoOp();
        return out;
    } catch (const std::exception& e) {
        set_err(err, err_len, e.what());
    } catch (...) {
        set_err(err, err_len, "unknown OpenColorIO error");
    }
    return nullptr;
}

// A LUT file in the viewer: colour space `src` → `lut_space` (the space the LUT expects), the
// LUT (.cube, .3dl, .csp, .spi1d/.spi3d, .clf, .cc/.cdl …, through OCIO's FileTransform), then
// either nothing more (`display` empty: the LUT's output is shown as display values, as most
// show LUTs from log to Rec.709 are), or back to `src` and through `display` / `view` (a look).
pc_ocio_proc* pc_ocio_lut_processor(const pc_ocio_config* c, const char* src, const char* lut, const char* lut_space, const char* display, const char* view,
                                    char* err, size_t err_len) {
    try {
        auto group = OCIO::GroupTransform::Create();
        auto to = OCIO::ColorSpaceTransform::Create();
        to->setSrc(src);
        to->setDst(lut_space);
        group->appendTransform(to);
        auto file = OCIO::FileTransform::Create();
        file->setSrc(lut);
        file->setInterpolation(OCIO::INTERP_BEST);
        group->appendTransform(file);
        if (display && *display) {
            auto back = OCIO::ColorSpaceTransform::Create();
            back->setSrc(lut_space);
            back->setDst(src);
            group->appendTransform(back);
            auto dvt = OCIO::DisplayViewTransform::Create();
            dvt->setSrc(src);
            dvt->setDisplay(display);
            dvt->setView(view);
            group->appendTransform(dvt);
        }
        auto proc = c->config->getProcessor(group);
        auto* out = new pc_ocio_proc();
        out->cpu = proc->getDefaultCPUProcessor();
        out->noop = proc->isNoOp();
        return out;
    } catch (const std::exception& e) {
        set_err(err, err_len, e.what());
    } catch (...) {
        set_err(err, err_len, "unknown OpenColorIO error");
    }
    return nullptr;
}

int pc_ocio_proc_is_noop(const pc_ocio_proc* p) { return p->noop ? 1 : 0; }

// Converts `width × height` packed float RGBA pixels in place (alpha passes through).
void pc_ocio_proc_apply_rgba(const pc_ocio_proc* p, float* data, long width, long height) {
    if (p->noop || width <= 0 || height <= 0) return;
    try {
        OCIO::PackedImageDesc img(data, width, height, 4);
        p->cpu->apply(img);
    } catch (...) {
        // Left unchanged: the caller validated the processor when it was made.
    }
}

void pc_ocio_proc_free(pc_ocio_proc* p) { delete p; }

}  // extern "C"

# DAUx Bundle Manifest — Specification v1

Status: **Stable draft** · Bundle format: `DAUx Audio Extension` · `formatVersion`: `1`

This document is the normative contract for the metadata that describes a DAUx Audio
Extension (`.axt`) **before its binary is loaded**: `manifest.json` on Windows and Linux,
`Contents/Info.plist` on Apple platforms, and the `[package.metadata.daux]` table that
generates both. `crates/daux-bundle` is the reference implementation; where the two
disagree, **this document wins** and `daux-bundle` is a bug.

`docs/specifications/abi-v1.md` remains the binary contract. Where this document and
abi-v1 describe the same value (identity, version, capabilities), **abi-v1 and the
compiled binary win** — see §8.

Key words **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, **MAY** are used as in RFC 2119.

---

## 1. Scope and design rules

A host scans hundreds of bundles at startup. Scanning MUST be possible without executing
plug-in code: opening a dynamic library runs static initialisers, maps pages, and pulls in
GPU and UI dependencies. The manifest exists so that a scanner can answer *"what is this,
can I load it, and do I want to?"* from a single small file.

That purpose fixes four design rules, and every decision below follows from them.

1. **The manifest is a pre-load index, not a database.** It carries identity, packaging
   and coarse capability bits. It never carries anything the binary itself reports
   (§7).
2. **The manifest is generated, never authored.** `[package.metadata.daux]` in the
   plug-in's own `Cargo.toml` is the single source of truth; `daux build` / `daux bundle`
   derive `manifest.json` and `Info.plist` from it (§2, §6). Duplicated truth drifts.
3. **The binary is authoritative.** Anything a manifest claims about the plug-in is a
   cached copy of what the binary says. When the two disagree, the copy is wrong (§8).
4. **Input is hostile.** A manifest is an attacker-reachable file in a directory the user
   double-clicked. Every parser limit in §10 is mandatory, and no parse path may panic.

Layout of the bundle itself (`Content/`, `Library/`, `Resources/`, `Contents/`) is fixed by
the bundle layout rules in §4; this document specifies only the metadata files and the
paths the metadata is allowed to name.

### 1.1 What this document does not cover

* The VST3 and CLAP exports. `daux build` generates their format-defined metadata
  (`moduleinfo.json`, the CLAP descriptor) from the *same* `[package.metadata.daux]`
  source of truth, but their shape is defined by those formats, not here.
* Signing, hashing and publisher identity. v1 deliberately has no signature keys.
  A future `formatVersion` MAY add them; readers MUST already tolerate their presence
  under the unknown-key rule (§9.1).
* Preset files and preset discovery.

---

## 2. Single source of truth: `[package.metadata.daux]`

> **The developer writes exactly one description of the plug-in, in `Cargo.toml`.**
> `manifest.json` and `Info.plist` are build outputs. A hand-edited `manifest.json` is a
> bug in the same way a hand-edited `target/` file is a bug: the next build overwrites it.

`daux build` MUST regenerate the metadata files on every invocation, unconditionally, and
MUST NOT merge, diff or preserve the previous contents of a generated file. A
`manifest.json` or `Info.plist` checked into the plug-in's source tree MUST be reported as
`DAUX-M203` (warning) by `daux validate`.

### 2.1 Location and spelling

The table lives at `[package.metadata.daux]` in the plug-in crate's `Cargo.toml`. Cargo
ignores `package.metadata` entirely, so no Cargo behaviour changes.

TOML keys are **kebab-case**; the corresponding JSON keys are **camelCase**. The mapping is
mechanical and normative:

```text
toml_key  →  json_key : split on '-', keep the first segment, upper-case the first
                        character of every following segment
                        e.g. sample-accurate-automation → sampleAccurateAutomation
```

A reader of `[package.metadata.daux]` MUST accept the kebab-case spelling and MUST also
accept the camelCase spelling of the same key. If both spellings of one key appear in the
same table, the reader MUST fail with `DAUX-M202`; it MUST NOT pick one.

Unknown keys inside `[package.metadata.daux]` MUST be reported as `DAUX-M205` (warning) —
they are almost always typos — and MUST NOT fail the build unless `--strict` is given.

### 2.2 The complete table

```toml
[package]
name        = "equzx"
version     = "1.0.0"
description = "Dynamic equalizer and spectral processor"
homepage    = "https://futureboard.studio/equzx"
license     = "MIT OR Apache-2.0"

[lib]
crate-type = ["cdylib"]

[dependencies]
daux-plugin = { version = "0.1", features = ["axt", "gpui", "wgpu"] }

[package.metadata.daux]
# ---- identity (permanent; see abi-v1 §14) ----------------------------------
id             = "studio.futureboard.equzx"   # REQUIRED
vendor         = "Futureboard Studio"          # REQUIRED
name           = "EQUZX"                       # default: package.name
version        = "1.0.0"                       # default: package.version, normalised
version-string = "1.0.0"                       # default: `version`
# ---- classification --------------------------------------------------------
category    = "effect"                         # default: "unknown"
features    = ["eq", "dynamics", "mastering"]  # default: []
description = "Dynamic equalizer and spectral processor"   # default: package.description
url         = "https://futureboard.studio/equzx"           # default: package.homepage
support-url = "https://futureboard.studio/support"         # default: ""
copyright   = "© 2026 Futureboard Studio"                  # default: ""
license     = "MIT OR Apache-2.0"                          # default: package.license
# ---- packaging -------------------------------------------------------------
bundle-name  = "EQUZX"                         # default: sanitised `name` (§4.3)
targets      = ["windows-x86_64", "linux-x86_64"]   # default: [TargetId::host()]
formats      = ["axt"]                         # default: ["axt"]; may add "vst3", "clap"
resources    = "assets"                        # default: none; source dir, copied to Resources/
library      = "vendor/lib"                    # default: none; source dir, copied to Library/{target}/
dependencies = ["dependency.dll"]              # default: []; expected contents of Library/{target}/
abi-version-minor  = 0                         # default: 0
macos-min-version  = "11.0"                    # default: "11.0"; Apple layout only

[package.metadata.daux.capabilities]           # every key defaults to false
audio-effect               = true
midi-input                 = true
sidechain                  = true
dynamic-buses              = true
sample-accurate-automation = true

[package.metadata.daux.graphics]               # omit the table entirely for a headless plug-in
enabled     = true                             # default: true when the table is present
framework   = "gpui"                           # REQUIRED when enabled
renderer    = "wgpu"                           # REQUIRED when enabled
presentation = "embedded-surface"              # default: "embedded-surface"
resizable   = true                             # default: false
width       = 1100                             # default: 800   (logical pixels)
height      = 700                              # default: 600
min-width   = 640                              # default: absent
min-height  = 400                              # default: absent
max-width   = 3840                             # default: absent
max-height  = 2160                             # default: absent
```

### 2.3 Requirements on the table

| Key                     | TOML type       | Required | Default                    | Rule                              |
| ----------------------- | --------------- | -------- | -------------------------- | --------------------------------- |
| `id`                    | string          | yes      | —                          | §3.4                              |
| `vendor`                | string          | yes      | —                          | ≤ 63 bytes, non-empty             |
| `name`                  | string          | no       | `package.name`             | ≤ 63 bytes, non-empty             |
| `version`               | string          | no       | `package.version`          | §3.5                              |
| `version-string`        | string          | no       | `version`                  | ≤ 63 bytes                        |
| `category`              | string          | no       | `"unknown"`                | §3.6                              |
| `features`              | array\<string\> | no       | `[]`                       | ≤ 32 tags, `[a-z0-9-]`, ≤ 31 B    |
| `description`           | string          | no       | `package.description`      | ≤ 255 bytes                       |
| `url`, `support-url`    | string          | no       | `package.homepage`, `""`   | ≤ 255 bytes                       |
| `copyright`             | string          | no       | `""`                       | ≤ 255 bytes                       |
| `license`               | string          | no       | `package.license`          | ≤ 63 bytes                        |
| `bundle-name`           | string          | no       | sanitised `name`           | §4.3                              |
| `targets`               | array\<string\> | no       | `[TargetId::host()]`       | §3.7, 1..=256, unique             |
| `formats`               | array\<string\> | no       | `["axt"]`                  | `axt` \| `vst3` \| `clap`         |
| `resources`             | string          | no       | absent                     | relative path inside the crate    |
| `library`               | string          | no       | absent                     | relative path inside the crate    |
| `dependencies`          | array\<string\> | no       | `[]`                       | §3.10                             |
| `abi-version-minor`     | integer         | no       | `0`                        | 0..=65535                         |
| `macos-min-version`     | string          | no       | `"11.0"`                   | `N.N` or `N.N.N`                  |
| `capabilities.*`        | boolean         | no       | `false`                    | §3.8                              |
| `graphics.*`            | mixed           | no       | absent                     | §3.9                              |

`resources` and `library` are **source** directories relative to the crate root: they say
where to copy *from*, not where the files land inside the bundle. Both MUST resolve inside
the crate directory after canonicalisation; a value that escapes it MUST fail with
`DAUX-M055`.

`daux build` MUST fail, not warn, when a required key is missing (`DAUX-M201`), when
`[package.metadata.daux]` is absent altogether (`DAUX-M200`), or when the crate's
`crate-type` does not include `cdylib` (`DAUX-M204`).

### 2.4 Derived version normalisation

`package.version` is a semver string and MAY carry a pre-release or build suffix
(`1.0.0-beta.2+ci.7`). `plugin.version` in the manifest is a numeric dotted string only
(§3.5). When `version` is derived from `package.version`, the generator MUST:

1. take `MAJOR.MINOR.PATCH` as `plugin.version`;
2. take the full original string, truncated on a character boundary to 63 bytes, as
   `plugin.versionString`;
3. emit a warning when a suffix was dropped, so the developer can set `version-string`
   deliberately.

`versionString` is display text only. Ordering comparisons anywhere in DAUx use the
four-component numeric `version`, per abi-v1 §2 (`DauxVersion` is ordered
lexicographically over `major, minor, patch, build`).

---

## 3. `manifest.json` v1 schema

The file is `manifest.json`, UTF-8, at the root of the `.axt` directory. It is REQUIRED in
the POSIX layout and MUST NOT be present in the Apple layout (§4).

### 3.1 Top level

| Key               | JSON type | Required | Default   | Notes                                             |
| ----------------- | --------- | -------- | --------- | ------------------------------------------------- |
| `format`          | string    | **yes**  | —         | MUST equal `"DAUx Audio Extension"` exactly       |
| `formatVersion`   | integer   | **yes**  | —         | `1` for this document; §9                         |
| `abiVersion`      | integer   | **yes**  | —         | ABI **major** version of the binaries; `1`        |
| `abiVersionMinor` | integer   | no       | `0`       | minimum ABI minor the binaries need               |
| `plugin`          | object    | **yes**  | —         | §3.2                                              |
| `targets`         | array     | **yes**  | —         | §3.7; 1..=256 unique target ids                   |
| `capabilities`    | object    | **yes**  | —         | §3.8; MAY be `{}`                                 |
| `graphics`        | object    | no       | absent    | §3.9; absent means "no editor"                    |
| `resources`       | object    | no       | see §3.11 | §3.11                                             |
| `dependencies`    | array     | no       | `[]`      | §3.10                                             |
| `generator`       | object    | no       | absent    | §3.12; informational, MUST NOT affect any decision |

Writers MUST emit the keys in the order of this table. Readers MUST NOT depend on order.

`format` is a fixed sentinel, not free text: it lets a reader reject an unrelated
`manifest.json` (npm, VS Code, a game asset pack) before doing any further work. A
mismatch is `DAUX-M005`.

### 3.2 `plugin`

| Key             | JSON type | Required | Default | Max bytes | ABI buffer      |
| --------------- | --------- | -------- | ------- | --------- | --------------- |
| `id`            | string    | **yes**  | —       | 127       | `DauxId[128]`   |
| `name`          | string    | **yes**  | —       | 63        | `DauxName[64]`  |
| `vendor`        | string    | **yes**  | —       | 63        | `DauxName[64]`  |
| `version`       | string    | **yes**  | —       | 63        | `DauxVersion`   |
| `description`   | string    | **yes**  | `""`    | 255       | `DauxText[256]` |
| `versionString` | string    | no       | `version` | 63      | `DauxName[64]`  |
| `category`      | string    | no       | `"unknown"` | 31    | `u32`           |
| `url`           | string    | no       | `""`    | 255       | `DauxText[256]` |
| `supportUrl`    | string    | no       | `""`    | 255       | `DauxText[256]` |
| `copyright`     | string    | no       | `""`    | 255       | `DauxText[256]` |
| `license`       | string    | no       | `""`    | 63        | `DauxName[64]`  |
| `features`      | array\<string\> | no | `[]`    | 32 tags   | `DauxText[256]` |

`description` is required-with-a-default: the key MAY be absent, and a reader MUST then use
`""`; writers MUST always emit it. The byte limits are chosen so that every value survives
a lossless round trip through the fixed ABI buffers of abi-v1 §2.1 with room for a NUL —
values are never truncated silently anywhere in the pipeline. A longer value is
`DAUX-M009` and rejects the manifest; it does not truncate.

`name`, `vendor` and `description` are UTF-8 and MAY contain non-ASCII characters. They
MUST NOT contain C0/C1 control characters (`U+0000`–`U+001F`, `U+007F`–`U+009F`) or
`U+2028`/`U+2029`. `id`, `version`, `category` and `features` entries are ASCII-only.

`features` is a set of free-form lower-case tags for search and filtering (`"eq"`,
`"dynamics"`, `"mastering"`). Each tag matches `[a-z0-9][a-z0-9-]*`, is ≤ 31 bytes, and the
list is ≤ 32 entries whose `";"`-joined form is ≤ 255 bytes, so it fits
`DauxPluginDescriptorV1::features`.

**Multi-plug-in bundles.** `plugin` describes the bundle's *principal* plug-in — the one
that names the bundle. A binary MAY export several plug-ins; the manifest MUST NOT
enumerate the others. Enumeration is the factory's job
(`DauxFactoryApiV1::plugin_count` / `descriptor`, abi-v1 §5), and it is cheap by
construction. The principal `id` MUST be one of the ids the factory exports
(`DAUX-M108`).

### 3.3 The stable prologue

`format`, `formatVersion`, `abiVersion`, `plugin.id`, `plugin.name` and `plugin.version`
are **frozen for all time across every future `formatVersion`**: same key names, same JSON
types, same meaning. Any DAUx reader, of any vintage, can therefore extract them from a
manifest it otherwise refuses and produce a useful diagnostic
(*"EQUZX 2.0.0 needs a newer host"*) instead of *"unparseable file"*. See §9.3.

### 3.4 Plug-in id format

```text
id      ::= label ( "." label )+
label   ::= alnum *( alnum / "-" / "_" )
alnum   ::= %x61-7A / %x30-39          ; lower-case ASCII letter or digit
```

* length 1..=127 bytes; at least one `.`; at least one ASCII letter overall;
* no leading or trailing `.`, no `..`, no empty label;
* lower-case only. Ids are compared **byte-for-byte**; a differing case is a *different*
  plug-in. Case is constrained to lower case so that case-insensitive filesystems and
  case-sensitive registries can never disagree about identity.
* Reverse-DNS is the required convention (`studio.futureboard.equzx`). A vendor MUST own
  the domain, or use a namespace nobody else will claim.

The id is permanent (abi-v1 §14). Changing it creates a different plug-in and silently
breaks every saved project that referenced the old one. A malformed id is `DAUX-M010`.

### 3.5 Version format

```text
version ::= u32 "." u32 "." u32 [ "." u32 ]
```

Decimal ASCII, no sign, no leading `+`, no whitespace, no pre-release or build suffix,
each component `0 ..= 4294967295`, no leading zeros except the literal `0`. A missing
fourth component means `build = 0`. Parsers MUST use checked integer parsing; a component
that overflows `u32` is `DAUX-M011`, never a wrapped value.

`macosMinVersion` (Apple layout only) uses the Apple form `N.N` or `N.N.N`.

### 3.6 Categories

| Slug           | `daux_core::Category` | abi-v1 §6.1 constant        |
| -------------- | --------------------- | --------------------------- |
| `unknown`      | `Unknown`             | `DAUX_CATEGORY_UNKNOWN` 0   |
| `effect`       | `Effect`              | `DAUX_CATEGORY_EFFECT` 1    |
| `instrument`   | `Instrument`          | `DAUX_CATEGORY_INSTRUMENT` 2 |
| `midi-effect`  | `MidiEffect`          | `DAUX_CATEGORY_MIDI_EFFECT` 3 |
| `analyzer`     | `Analyzer`            | `DAUX_CATEGORY_ANALYZER` 4  |
| `generator`    | `Generator`           | `DAUX_CATEGORY_GENERATOR` 5 |
| `utility`      | `Utility`             | `DAUX_CATEGORY_UTILITY` 6   |

An unrecognised slug is `DAUX-M015`. A reader MUST NOT silently map it to `unknown`: a
typo'd category must be visible at build time, not at scan time on a user's machine.

### 3.7 Target identifiers

```text
target ::= os "-" arch
os     ::= "windows" / "linux" / "macos"
arch   ::= "x86_64" / "aarch64" / "universal"          ; "universal" only with "macos"
```

Lower-case ASCII, ≤ 32 bytes. The v1 registry:

| Target id         | Dylib extension | Rust target triples                                                |
| ----------------- | --------------- | ------------------------------------------------------------------ |
| `windows-x86_64`  | `dll`           | `x86_64-pc-windows-msvc`, `x86_64-pc-windows-gnu`                   |
| `windows-aarch64` | `dll`           | `aarch64-pc-windows-msvc`                                           |
| `linux-x86_64`    | `so`            | `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`             |
| `linux-aarch64`   | `so`            | `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`           |
| `macos-x86_64`    | `dylib`         | `x86_64-apple-darwin`                                               |
| `macos-aarch64`   | `dylib`         | `aarch64-apple-darwin`                                              |
| `macos-universal` | `dylib`         | `x86_64-apple-darwin` + `aarch64-apple-darwin` (`lipo`)             |

The array MUST contain 1..=256 entries and MUST NOT contain duplicates (`DAUX-M013`).

A reader that meets a **well-formed but unregistered** target id (a future
`linux-riscv64`) MUST accept the manifest and ignore that entry — it simply has no binary
it can load for it. A **malformed** target id (uppercase, three segments, 40 bytes) is
`DAUX-M013` and rejects the manifest. This split is what lets a new architecture ship
without invalidating every existing host.

`TargetId::host()` resolves the running platform to one of these ids; on macOS it MUST
match `macos-universal` as well as the native architecture id.

### 3.8 `capabilities`

A flat object of booleans mirroring the `DAUX_CAP_*` bitset of abi-v1 §6.2. Every key is
optional and defaults to `false`; `{}` is legal and means "nothing declared".

| JSON key                   | abi-v1 bit                      |
| -------------------------- | ------------------------------- |
| `audioEffect`              | `DAUX_CAP_AUDIO_EFFECT`         |
| `instrument`               | `DAUX_CAP_INSTRUMENT`           |
| `midiEffect`               | `DAUX_CAP_MIDI_EFFECT`          |
| `analyzer`                 | `DAUX_CAP_ANALYZER`             |
| `midiInput`                | `DAUX_CAP_MIDI_INPUT`           |
| `midiOutput`               | `DAUX_CAP_MIDI_OUTPUT`          |
| `midi2`                    | `DAUX_CAP_MIDI2`                |
| `sidechain`                | `DAUX_CAP_SIDECHAIN`            |
| `dynamicBuses`             | `DAUX_CAP_DYNAMIC_BUSES`        |
| `sampleAccurateAutomation` | `DAUX_CAP_SAMPLE_ACCURATE_AUTO` |
| `noteExpression`           | `DAUX_CAP_NOTE_EXPRESSION`      |
| `hasGui`                   | `DAUX_CAP_HAS_GUI`              |
| `requiresGui`              | `DAUX_CAP_REQUIRES_GUI`         |
| `sharedTextureGui`         | `DAUX_CAP_SHARED_TEXTURE_GUI`   |
| `offlineRender`            | `DAUX_CAP_OFFLINE_RENDER`       |
| `hardRealtime`             | `DAUX_CAP_HARD_REALTIME`        |
| `sandboxSafe`              | `DAUX_CAP_SANDBOX_SAFE`         |
| `stereoOnly`               | `DAUX_CAP_STEREO_ONLY`          |
| `latencyDynamic`           | `DAUX_CAP_LATENCY_DYNAMIC`      |
| `tailInfinite`             | `DAUX_CAP_TAIL_INFINITE`        |

Values MUST be JSON booleans. `0`, `1`, `"true"` and `null` are `DAUX-M008`. Unknown
capability names MUST be ignored (§9.1) — a v1 host simply cannot act on a capability it
has never heard of. The object MUST NOT contain more than 256 keys.

Consistency rules checked by `daux validate`:

* `requiresGui` implies `hasGui` (`DAUX-M107`);
* `sharedTextureGui` implies `hasGui` (`DAUX-M107`);
* `graphics.enabled == true` implies `hasGui` (`DAUX-M107`);
* `instrument` and `midiEffect` SHOULD agree with `plugin.category`; a disagreement is a
  warning (`DAUX-M104`), because a plug-in can legitimately be an instrument that also
  processes audio.

### 3.9 `graphics`

Absent `graphics` means the bundle declares no editor. When present:

| Key            | JSON type | Required | Default              | Range / values                                                              |
| -------------- | --------- | -------- | -------------------- | --------------------------------------------------------------------------- |
| `enabled`      | boolean   | no       | `true`               |                                                                              |
| `framework`    | string    | when enabled | —                | `egui` \| `gpui` \| `custom`                                                 |
| `renderer`     | string    | when enabled | —                | `wgpu` \| `opengl` \| `software`                                             |
| `presentation` | string    | no       | `embedded-surface`   | `native-window` \| `embedded-surface` \| `shared-texture` \| `external-window` |
| `resizable`    | boolean   | no       | `false`              |                                                                              |
| `width`        | integer   | no       | `800`                | 1..=16384 logical pixels                                                     |
| `height`       | integer   | no       | `600`                | 1..=16384                                                                    |
| `minWidth`     | integer   | no       | absent               | 1..=16384, ≤ `width`                                                         |
| `minHeight`    | integer   | no       | absent               | 1..=16384, ≤ `height`                                                        |
| `maxWidth`     | integer   | no       | absent               | ≥ `width`, ≤ 16384                                                           |
| `maxHeight`    | integer   | no       | absent               | ≥ `height`, ≤ 16384                                                          |

Sizes are **logical** pixels (`daux_graphics::LogicalSize`), integral in the manifest; the
runtime works in `f64` and applies the HiDPI scale factor. Out-of-range or inverted bounds
are `DAUX-M016`.

Everything here is a **hint for the pre-load phase**: it lets a host decide whether to
reserve a GPU adapter, size a placeholder window, or skip the bundle entirely in a
headless render farm. The plug-in's `GraphicDescriptor`, returned after load, is
authoritative for the actual editor (§7).

### 3.10 `dependencies`

An array of bare file names that MUST exist in `Library/{target}/` for every declared
target (POSIX layout) or in `Contents/Frameworks/` (Apple layout).

* ≤ 256 entries, each ≤ 255 bytes, unique;
* a single path segment: no `/`, `\`, `:`, `*`, `?`, `"`, `<`, `>`, `|`, no control
  characters, not `.` or `..`, no leading or trailing space or dot;
* MUST NOT be a Windows reserved device name (`CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`,
  `LPT0`–`LPT9`), with or without an extension, in any case.

The list is declarative: `daux validate` checks that each name is present (`DAUX-M053`).
It never drives loading. Dependency resolution is the loader's business, and per §26 of
the design brief it MUST use scoped mechanisms (`AddDllDirectory` +
`LOAD_LIBRARY_SEARCH_USER_DIRS`, `$ORIGIN`, `@loader_path`) and MUST NOT mutate `PATH`,
`LD_LIBRARY_PATH` or `DYLD_LIBRARY_PATH`.

### 3.11 `resources`

| Key          | JSON type | Required | Default       | Rule                        |
| ------------ | --------- | -------- | ------------- | --------------------------- |
| `dir`        | string    | no       | `"Resources"` | single segment, see below   |
| `libraryDir` | string    | no       | `"Library"`   | single segment, see below   |

Both name a directory **at the root of the bundle**. Each MUST be a single path segment of
1..=64 bytes matching `[A-Za-z0-9._-]+`, MUST NOT be `.` or `..`, MUST NOT start or end
with `.` or a space, and MUST NOT be a Windows reserved device name (`DAUX-M017`). They
exist so that an unusual product layout stays describable; changing them is discouraged
and `daux build` emits the defaults.

These keys are ignored in the Apple layout, where the directories are fixed by Apple
convention (`Contents/Resources`, `Contents/Frameworks`).

Resource *lookup* is always logical and always confined to the bundle:
`ResourceDir::read("Shaders/spectrum.wgsl")`. `..`, absolute paths, drive letters, UNC
prefixes, Windows device names and symlinks whose canonical target lies outside the bundle
MUST be rejected with `PathEscape` / `DAUX-M055`.

### 3.12 `generator`

| Key       | JSON type | Notes                                            |
| --------- | --------- | ------------------------------------------------ |
| `name`    | string    | tool name, e.g. `"daux"`                         |
| `version` | string    | tool version                                     |
| `note`    | string    | free text                                        |

Provenance only. `daux build` SHOULD emit it. A reader MUST NOT let any value here change
its behaviour, and MUST tolerate the whole object being absent, empty or unrecognised.
JSON has no comments, so this object is also where the "do not edit" notice lives.

### 3.13 Deterministic output

Two builds of the same source MUST produce byte-identical metadata files — reproducible
builds and any future hash manifest depend on it. Writers MUST:

* emit UTF-8 **without** a BOM, LF line endings, exactly one trailing LF;
* indent with two spaces, one key per line;
* emit keys in the order given in §3.1/§3.2, and capability and target entries in the
  order given in §3.8/§3.7 (not in hash order);
* emit integers without exponents or fractional parts;
* escape only what RFC 8259 requires, plus `U+007F`; MUST NOT `\u`-escape other non-ASCII
  characters.

---

## 4. Where the metadata file lives

### 4.1 POSIX layout (Windows, Linux)

```text
EQUZX.axt/
├─ manifest.json                     ← REQUIRED, this specification
├─ Content/
│  └─ windows-x86_64/
│     └─ EQUZX.dll
├─ Library/
│  └─ windows-x86_64/
│     └─ dependency.dll
└─ Resources/
   ├─ Shaders/
   ├─ Images/
   └─ Presets/
```

### 4.2 Apple layout (macOS)

```text
EQUZX.axt/
└─ Contents/
   ├─ Info.plist                     ← REQUIRED, §6
   ├─ MacOS/
   │  └─ EQUZX                       ← Mach-O, possibly universal
   ├─ Frameworks/
   └─ Resources/
```

A bundle uses exactly one layout. `manifest.json` MUST NOT appear in an Apple-layout
bundle and `Contents/Info.plist` MUST NOT appear in a POSIX-layout bundle (`DAUX-M056`);
a reader that finds both MUST reject the bundle rather than pick one, because the two
files are exactly the kind of duplicated truth this specification exists to prevent.

A bundle MUST NOT mix Apple and non-Apple targets (`DAUX-M059`): Apple codesigning and
notarisation require the `Contents/` layout, which cannot host `Content/{target}/`. A
cross-platform product ships one `.axt` per platform family.

### 4.3 Names inside the bundle

* The bundle directory is `{BundleName}.axt` (`DAUX-M057` otherwise).
* `BundleName` defaults to `plugin.name` sanitised: keep `[A-Za-z0-9 ._-]`, drop
  everything else, collapse runs of spaces, trim leading/trailing spaces and dots,
  truncate on a character boundary to 64 bytes; if the result is empty, use the last
  label of `plugin.id`. It MUST NOT be a Windows reserved device name (`DAUX-M058`).
* The binary is `Content/{target}/{BundleName}.{dll|so|dylib}` — note that on Linux there
  is **no** `lib` prefix. Cargo emits `libequzx.so` / `equzx.dll`; `daux build` renames on
  copy.
* Resolution rule for readers: look for `{BundleName}.{ext}` first; if it is absent, accept
  the single file in that directory carrying the target's dylib extension. Zero matches is
  `DAUX-M050`; two or more is `DAUX-M052` — a bundle MUST NOT be ambiguous about which
  library the host loads.
* In the Apple layout the binary is `Contents/MacOS/{CFBundleExecutable}` for every
  declared `macos-*` target, and `DAUxTargets` lists the architectures actually present in
  the Mach-O.
* `daux build` writes to `target/daux/{profile}/{format}/{BundleName}.axt`.

---

## 5. Examples

### 5.1 Minimal

Everything optional omitted. This is a legal, complete v1 manifest.

```json
{
  "format": "DAUx Audio Extension",
  "formatVersion": 1,
  "abiVersion": 1,
  "plugin": {
    "id": "studio.futureboard.gain",
    "name": "Gain",
    "vendor": "Futureboard Studio",
    "version": "0.1.0",
    "description": ""
  },
  "targets": ["windows-x86_64"],
  "capabilities": {}
}
```

Reading it yields: category `unknown`, no declared capabilities, no editor,
`Resources/`, `Library/`, no dependencies, `abiVersionMinor` 0.

### 5.2 Canonical worked example

The reference manifest from §25 of the design brief, unchanged. It is valid v1: every key
it uses is defined above, and every key it omits has a default.

```json
{
  "format": "DAUx Audio Extension",
  "formatVersion": 1,
  "abiVersion": 1,

  "plugin": {
    "id": "studio.futureboard.equzx",
    "name": "EQUZX",
    "vendor": "Futureboard Studio",
    "version": "1.0.0",
    "description": "Dynamic equalizer and spectral processor"
  },

  "targets": [
    "windows-x86_64",
    "linux-x86_64"
  ],

  "capabilities": {
    "audioEffect": true,
    "instrument": false,
    "midiInput": true,
    "midiOutput": false,
    "sidechain": true,
    "dynamicBuses": true,
    "sampleAccurateAutomation": true
  },

  "graphics": {
    "enabled": true,
    "framework": "gpui",
    "renderer": "wgpu",
    "resizable": true
  }
}
```

Note what is **not** in it: no parameter list, no bus layout, no channel counts, no
latency, no state schema version, no entry-point symbol name. See §7.

### 5.3 Fully populated

What `daux build` actually emits for the `[package.metadata.daux]` table of §2.2.

```json
{
  "format": "DAUx Audio Extension",
  "formatVersion": 1,
  "abiVersion": 1,
  "abiVersionMinor": 0,
  "plugin": {
    "id": "studio.futureboard.equzx",
    "name": "EQUZX",
    "vendor": "Futureboard Studio",
    "version": "1.0.0",
    "description": "Dynamic equalizer and spectral processor",
    "versionString": "1.0.0",
    "category": "effect",
    "url": "https://futureboard.studio/equzx",
    "supportUrl": "https://futureboard.studio/support",
    "copyright": "© 2026 Futureboard Studio",
    "license": "MIT OR Apache-2.0",
    "features": ["eq", "dynamics", "mastering"]
  },
  "targets": ["windows-x86_64", "linux-x86_64"],
  "capabilities": {
    "audioEffect": true,
    "midiInput": true,
    "sidechain": true,
    "dynamicBuses": true,
    "sampleAccurateAutomation": true,
    "hasGui": true
  },
  "graphics": {
    "enabled": true,
    "framework": "gpui",
    "renderer": "wgpu",
    "presentation": "embedded-surface",
    "resizable": true,
    "width": 1100,
    "height": 700,
    "minWidth": 640,
    "minHeight": 400,
    "maxWidth": 3840,
    "maxHeight": 2160
  },
  "resources": {
    "dir": "Resources",
    "libraryDir": "Library"
  },
  "dependencies": ["dependency.dll"],
  "generator": {
    "name": "daux",
    "version": "0.1.0",
    "note": "Generated from [package.metadata.daux]; do not edit by hand."
  }
}
```

`hasGui` appears in `capabilities` even though the developer never wrote it: it is derived
from the presence of an enabled `[package.metadata.daux.graphics]` table (§5.4).

### 5.4 Generation rules — `[package.metadata.daux]` → `manifest.json`

| manifest.json                | source                                                                 |
| ---------------------------- | ---------------------------------------------------------------------- |
| `format`                     | constant `"DAUx Audio Extension"`                                       |
| `formatVersion`              | constant `1`                                                            |
| `abiVersion`                 | `DAUX_ABI_VERSION_MAJOR` the SDK was built against                      |
| `abiVersionMinor`            | `abi-version-minor`, default `0`                                        |
| `plugin.id`                  | `id` (verbatim, validated against §3.4)                                 |
| `plugin.name`                | `name` ?? `package.name`                                                |
| `plugin.vendor`              | `vendor`                                                                |
| `plugin.version`             | `version` ?? `package.version`, normalised per §2.4                     |
| `plugin.versionString`       | `version-string` ?? original `package.version` string                   |
| `plugin.category`            | `category` ?? `"unknown"`                                               |
| `plugin.description`         | `description` ?? `package.description` ?? `""`                          |
| `plugin.url`                 | `url` ?? `package.homepage` ?? `""`                                     |
| `plugin.supportUrl`          | `support-url` ?? `""`                                                   |
| `plugin.copyright`           | `copyright` ?? `""`                                                     |
| `plugin.license`             | `license` ?? `package.license` ?? `""`                                  |
| `plugin.features`            | `features` ?? `[]`                                                      |
| `targets`                    | `targets` ?? `[TargetId::host()]`, filtered to the targets actually built |
| `capabilities.*`             | `capabilities.*`, plus derived bits below                               |
| `graphics`                   | the `graphics` table, with §3.9 defaults filled in; omitted when the table is absent or `enabled = false` |
| `resources.dir`              | constant `"Resources"`                                                  |
| `resources.libraryDir`       | constant `"Library"`                                                    |
| `dependencies`               | `dependencies` ?? `[]`                                                  |
| `generator`                  | the tool's own name and version                                         |

Derived capability bits — the generator MUST set these and MUST NOT require the developer
to keep them in sync by hand:

| Bit             | Derived when                                                          |
| --------------- | --------------------------------------------------------------------- |
| `hasGui`        | an enabled `graphics` table is present                                 |
| `sharedTextureGui` | `graphics.presentation == "shared-texture"`                        |
| `audioEffect`   | `category == "effect"` and the developer set no capability at all      |
| `instrument`    | `category == "instrument"` and the developer set no capability at all  |
| `midiEffect`    | `category == "midi-effect"` and the developer set no capability at all |
| `analyzer`      | `category == "analyzer"` and the developer set no capability at all    |

The `category`-derived defaults apply only when the `[capabilities]` table is entirely
absent; once the developer writes one capability, the table is taken as complete and only
`hasGui` / `sharedTextureGui` are still derived. `targets` is intersected with what the
build actually produced: a manifest MUST NOT declare a target whose binary is missing
(`DAUX-M050`), so `cargo build` for one host does not emit a manifest promising four.

---

## 6. `Info.plist` (Apple layout)

`Contents/Info.plist` carries the same information as `manifest.json` through standard
Apple bundle keys plus a `DAUx`-namespaced set. It is generated from the same
`[package.metadata.daux]` table, by the same rules, and is equally never hand-edited.

Readers MUST accept XML plists (UTF-8) and binary plist v0 — Xcode and codesigning
pipelines routinely rewrite XML plists as binary. `daux bundle` MUST write XML.

### 6.1 Standard keys

| Key                            | plist type | Source                                              |
| ------------------------------ | ---------- | --------------------------------------------------- |
| `CFBundleIdentifier`           | string     | `plugin.id`                                         |
| `CFBundleName`                 | string     | `BundleName` (§4.3)                                 |
| `CFBundleDisplayName`          | string     | `plugin.name`                                       |
| `CFBundleExecutable`           | string     | `BundleName`                                        |
| `CFBundlePackageType`          | string     | constant `"BNDL"`                                   |
| `CFBundleShortVersionString`   | string     | `MAJOR.MINOR.PATCH` of `plugin.version`             |
| `CFBundleVersion`              | string     | decimal `build` when `build > 0`, else the same as `CFBundleShortVersionString` |
| `CFBundleInfoDictionaryVersion`| string     | constant `"6.0"`                                    |
| `CFBundleSupportedPlatforms`   | array      | `["MacOSX"]`                                        |
| `LSMinimumSystemVersion`       | string     | `macos-min-version`, default `"11.0"`               |
| `NSHumanReadableCopyright`     | string     | `plugin.copyright` (omitted when empty)             |

`CFBundleName` is deliberately the (short) bundle name and `CFBundleDisplayName` the full
product name: Apple's guidance keeps `CFBundleName` brief, while the DAUx display name has
no such limit.

### 6.2 DAUx keys

| Key                 | plist type       | Source / value                                        | Required |
| ------------------- | ---------------- | ----------------------------------------------------- | -------- |
| `DAUxFormatVersion` | integer          | `1`                                                   | yes      |
| `DAUxAbiVersion`    | integer          | ABI major version, `1`                                | yes      |
| `DAUxPluginType`    | string           | category slug (§3.6)                                  | yes      |
| `DAUxVendor`        | string           | `plugin.vendor`                                       | yes      |
| `DAUxEntrypoint`    | string           | `"daux_plugin_entry_v1"`                              | yes      |
| `DAUxCapabilities`  | dict\<bool\>     | the `capabilities` object verbatim (§3.8)             | yes      |
| `DAUxTargets`       | array\<string\>  | the `targets` array (§3.7)                            | yes      |
| `DAUxAbiVersionMinor` | integer        | `abiVersionMinor`                                     | no (0)   |
| `DAUxVersionString` | string           | `plugin.versionString`                                | no       |
| `DAUxDescription`   | string           | `plugin.description`                                  | no (`""`)|
| `DAUxUrl`           | string           | `plugin.url`                                          | no       |
| `DAUxSupportUrl`    | string           | `plugin.supportUrl`                                   | no       |
| `DAUxLicense`       | string           | `plugin.license`                                      | no       |
| `DAUxFeatures`      | array\<string\>  | `plugin.features`                                     | no (`[]`)|
| `DAUxGraphics`      | dict             | the `graphics` object (§3.9), keys unchanged          | no       |
| `DAUxDependencies`  | array\<string\>  | `dependencies`; resolved in `Contents/Frameworks`     | no (`[]`)|
| `DAUxGenerator`     | string           | `"{name} {version}"`                                  | no       |

`DAUxEntrypoint` is redundant with abi-v1 §4 (the symbol name is fixed by the ABI major
version) but is written anyway: a macOS host that inspects a bundle it cannot load still
learns which entry generation the binary was built for.

`DAUxTargets` is REQUIRED because `BundleMetadata` (§7.3) must be constructible from
either metadata file without loading anything.

Every value carrying text obeys the same byte limits and character rules as its
`manifest.json` counterpart (§3.2, §10). A key with the wrong plist type is `DAUX-M008`.

### 6.3 Full example

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Generated from [package.metadata.daux] by daux 0.1.0; do not edit by hand. -->
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>studio.futureboard.equzx</string>
	<key>CFBundleName</key>
	<string>EQUZX</string>
	<key>CFBundleDisplayName</key>
	<string>EQUZX</string>
	<key>CFBundleExecutable</key>
	<string>EQUZX</string>
	<key>CFBundlePackageType</key>
	<string>BNDL</string>
	<key>CFBundleShortVersionString</key>
	<string>1.0.0</string>
	<key>CFBundleVersion</key>
	<string>1.0.0</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleSupportedPlatforms</key>
	<array>
		<string>MacOSX</string>
	</array>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHumanReadableCopyright</key>
	<string>© 2026 Futureboard Studio</string>

	<key>DAUxFormatVersion</key>
	<integer>1</integer>
	<key>DAUxAbiVersion</key>
	<integer>1</integer>
	<key>DAUxAbiVersionMinor</key>
	<integer>0</integer>
	<key>DAUxPluginType</key>
	<string>effect</string>
	<key>DAUxVendor</key>
	<string>Futureboard Studio</string>
	<key>DAUxEntrypoint</key>
	<string>daux_plugin_entry_v1</string>
	<key>DAUxVersionString</key>
	<string>1.0.0</string>
	<key>DAUxDescription</key>
	<string>Dynamic equalizer and spectral processor</string>
	<key>DAUxUrl</key>
	<string>https://futureboard.studio/equzx</string>
	<key>DAUxSupportUrl</key>
	<string>https://futureboard.studio/support</string>
	<key>DAUxLicense</key>
	<string>MIT OR Apache-2.0</string>
	<key>DAUxFeatures</key>
	<array>
		<string>eq</string>
		<string>dynamics</string>
		<string>mastering</string>
	</array>
	<key>DAUxTargets</key>
	<array>
		<string>macos-universal</string>
	</array>
	<key>DAUxCapabilities</key>
	<dict>
		<key>audioEffect</key>
		<true/>
		<key>midiInput</key>
		<true/>
		<key>sidechain</key>
		<true/>
		<key>dynamicBuses</key>
		<true/>
		<key>sampleAccurateAutomation</key>
		<true/>
		<key>hasGui</key>
		<true/>
	</dict>
	<key>DAUxGraphics</key>
	<dict>
		<key>enabled</key>
		<true/>
		<key>framework</key>
		<string>gpui</string>
		<key>renderer</key>
		<string>wgpu</string>
		<key>presentation</key>
		<string>embedded-surface</string>
		<key>resizable</key>
		<true/>
		<key>width</key>
		<integer>1100</integer>
		<key>height</key>
		<integer>700</integer>
	</dict>
	<key>DAUxDependencies</key>
	<array/>
	<key>DAUxGenerator</key>
	<string>daux 0.1.0</string>
</dict>
</plist>
```

---

## 7. Authority: who owns which fact

### 7.1 The authority table

| Fact                                        | Authoritative source                              | In the manifest? |
| ------------------------------------------- | ------------------------------------------------- | ---------------- |
| Bundle layout, target list, binary paths    | manifest / `Info.plist`                           | yes — only here  |
| Resource and dependency directories         | manifest / `Info.plist`                           | yes — only here  |
| Which plug-ins the binary exports           | `DauxFactoryApiV1::plugin_count` / `descriptor`   | no               |
| Plug-in id, name, vendor, version, category | `DauxPluginDescriptorV1` (binary)                 | cached copy      |
| Capability bits                             | `DauxPluginDescriptorV1::capabilities` (binary)   | cached copy      |
| ABI major/minor the binary needs            | `DauxPluginEntryV1` + descriptor `min_abi_*`      | cached copy      |
| Parameter count, ids, ranges, flags, text   | `daux.params/1` (binary)                          | **never**        |
| Bus topology, channel counts, layouts       | `daux.audio-ports/1` (binary)                     | **never**        |
| Event/note port topology                    | `daux.note-ports/1` (binary)                      | **never**        |
| Latency                                     | `daux.latency/1` (binary, post-`activate`)        | **never**        |
| Tail length                                 | `daux.tail/1` (binary, dynamic)                   | **never**        |
| State schema version                        | `DauxPluginDescriptorV1::state_schema_version`    | **never**        |
| Editor existence                            | `DAUX_CAP_HAS_GUI` (binary)                       | hint (`graphics`) |
| Editor size, resize policy, aspect          | `GraphicDescriptor` (binary, post-load)           | hint (`graphics`) |

### 7.2 Why the manifest omits the interesting parts

The brief's instruction — *"do not overstuff the manifest with information that is better
queried from the lightweight binary descriptor"* — is a correctness requirement, not
tidiness:

* **Those values are not constants.** Bus layouts change when the host proposes a
  different configuration and the plug-in accepts it (`accepts_bus_layout`). Latency
  changes with `prepare` and with parameter values (`DAUX_CAP_LATENCY_DYNAMIC`). Tail
  changes per block. Parameter metadata can change and be re-announced through
  `daux.host.params/1::rescan`. A file written at build time cannot describe any of them,
  and a host that believed it would mis-compensate delay or mis-route audio.
* **A cached copy that can go stale must be cheap to invalidate.** Identity and
  capabilities are stable for the lifetime of a build, so caching them is safe and the
  cross-check in §8 is a byte comparison. Parameter tables are neither.
* **Enumerating the binary is already cheap.** abi-v1 §5 requires `descriptor` to be
  lightweight: no DSP instantiation, no resource loading, no GPU. The manifest only has to
  get the host as far as *deciding to call it*.
* **A manifest is user-writable; DSP truth must not be.** A stale or hand-tampered
  manifest must never be able to make a host believe something false about the signal
  path. Restricting the manifest to packaging facts and a checkable identity copy makes
  that structurally impossible.

### 7.3 `BundleMetadata` normalisation

`daux_bundle::BundleMetadata` is the layout-independent view produced from *either* file.
Both readers MUST produce identical values for the same logical bundle:

| `BundleMetadata` field | manifest.json           | Info.plist                                 |
| ---------------------- | ----------------------- | ------------------------------------------ |
| `id`                   | `plugin.id`             | `CFBundleIdentifier`                       |
| `name`                 | `plugin.name`           | `CFBundleDisplayName` ?? `CFBundleName`    |
| `vendor`               | `plugin.vendor`         | `DAUxVendor`                               |
| `version`              | `plugin.version`        | `CFBundleShortVersionString` (+ `CFBundleVersion` as build when numeric) |
| `description`          | `plugin.description`    | `DAUxDescription` ?? `""`                  |
| `format_version`       | `formatVersion`         | `DAUxFormatVersion`                        |
| `abi_version`          | `abiVersion`            | `DAUxAbiVersion`                           |
| `targets`              | `targets`               | `DAUxTargets`                              |
| `capabilities`         | `capabilities`          | `DAUxCapabilities`                         |
| `graphics`             | `graphics`              | `DAUxGraphics`                             |

---

## 8. Manifest ↔ binary disagreement

A generated manifest can still be wrong: someone edited it, the bundle was assembled by
hand, a build copied a stale binary, or a repackager swapped one plug-in for another.

### 8.1 The cross-check set

After a scanner has loaded the module and read `DauxPluginDescriptorV1` for the principal
plug-in, it **MUST** compare the following, and MUST record every difference on the
`ScanEntry` as a `ValidationIssue`:

| # | Manifest value        | Binary value                                    | Comparison              | Code       | Severity |
| - | --------------------- | ----------------------------------------------- | ----------------------- | ---------- | -------- |
| 1 | `plugin.id`           | `descriptor.id`                                 | byte-for-byte           | `DAUX-M100` | Error, fatal |
| 2 | `plugin.name`         | `descriptor.name`                               | byte-for-byte           | `DAUX-M101` | Error    |
| 3 | `plugin.vendor`       | `descriptor.vendor`                             | byte-for-byte           | `DAUX-M102` | Error    |
| 4 | `plugin.version`      | `descriptor.version`                            | all four components     | `DAUX-M103` | Error    |
| 5 | `plugin.category`     | `descriptor.category`                           | slug ↔ constant (§3.6)  | `DAUX-M104` | Warning  |
| 6 | `capabilities`        | `descriptor.capabilities`                       | full `u64` bitset       | `DAUX-M105` | Error    |
| 7 | `abiVersion`          | `entry.abi_version_major`                       | equality                | `DAUX-M106` | Error    |
| 8 | `graphics.enabled`    | `DAUX_CAP_HAS_GUI`                              | equality                | `DAUX-M107` | Error    |
| 9 | `plugin.id`           | ∈ ids exported by the factory                   | membership              | `DAUX-M108` | Error, fatal |

`versionString`, `description`, `url`, `supportUrl`, `copyright`, `license` and `features`
are **not** cross-checked: they are display text, and a bundler is allowed to localise or
re-word them.

### 8.2 What each consumer does about it

**The binary always wins.** Once a module is loaded, every host-visible value comes from
the descriptor. The manifest's job is over the moment the library is open. A host MUST NOT
present manifest values alongside, or in place of, descriptor values after load.

* **Scanner (`daux-scan`)** — MUST perform §8.1 and MUST attach the issues to the
  `ScanEntry`. For the two *fatal* rows (`DAUX-M100`, `DAUX-M108`), the entry MUST NOT be
  registered: identity is what saved projects reference, so a bundle that lies about its
  identity is unusable, not merely untidy. For every other row the entry IS registered,
  populated from the descriptor, and flagged. The scanner MUST NOT rewrite the manifest to
  "repair" it: the bundle may be read-only, signed, or shared, and silent repair hides a
  real packaging bug.
* **`daux validate`** — MUST report every row of §8.1 as described, MUST print the manifest
  value and the binary value side by side, and MUST exit non-zero when any `Error`-severity
  issue was produced (warnings alone exit zero unless `--deny-warnings` is given).
  This is the command that turns a silent drift into a build failure.
* **`daux inspect`** — MUST show both sources when they differ, and MUST label which is
  which.
* **Cache** — a `ScanEntry` fingerprint MUST cover the metadata file's bytes *and* the
  binary's size and modification time, so that editing either one invalidates the entry.

### 8.3 Pre-load rejection

`abiVersion` is also used *before* loading: when it names a major version the host does not
implement, the scanner MUST skip the bundle without opening the library and record a
warning ("requires a newer host"). This is the whole point of having the value in a file —
it saves a `dlopen` of a module that would be rejected at abi-v1 §3 anyway. A manifest that
under-reports (`1` when the binary is `2`) is caught by row 7 at load time.

---

## 9. Forward compatibility

Five version numbers exist in DAUxPlug and they move independently (design brief §54):

| Axis                 | Where it lives                                       | Bumped when                                  |
| -------------------- | ---------------------------------------------------- | -------------------------------------------- |
| SDK version          | `Cargo.toml` `[workspace.package] version`           | any SDK release                              |
| Native ABI version   | `DAUX_ABI_VERSION_MAJOR/MINOR`, `DauxPluginEntryV1`  | abi-v1 §3 rules                              |
| AXT format version   | `formatVersion` / `DAUxFormatVersion` — this document | §9.2                                        |
| Plug-in version      | `plugin.version`                                     | the developer's release                      |
| State schema version | `descriptor.state_schema_version`                    | the plug-in's persisted format changes       |

Never conflate them. A new SDK release does not bump `formatVersion`; a new ABI minor does
not bump `formatVersion`; a plug-in shipping 2.0 does not bump anything but its own version.

### 9.1 Unknown keys: preserve and ignore

A reader MUST NOT fail because of a key it does not recognise. This applies at every level:
top-level keys, keys inside `plugin`, `graphics` and `resources`, names inside
`capabilities`, well-formed-but-unregistered target ids (§3.7), and `DAUx*` keys in an
`Info.plist`. The reader ignores them for every decision it makes.

A reader that *rewrites* a metadata file MUST preserve the unknown keys it read, in their
original position where the format allows it. `daux-bundle`'s `Manifest` therefore captures
unrecognised top-level keys rather than discarding them. `daux build` is exempt: it
regenerates from `[package.metadata.daux]` and by definition has nothing to preserve.

Ignoring is not the same as accepting garbage: a *known* key with a wrong type, an
out-of-range value, or an over-long string is still an error (§10). Unknown-key tolerance
buys forward compatibility; it does not weaken validation of what v1 does define.

### 9.2 When `formatVersion` must be bumped

`formatVersion` is an integer counter of **breaking** changes, not a semver. It stays `1`
for changes a v1 reader can safely ignore or already handles:

* adding a new optional key with a defined default;
* adding a new name to `capabilities`;
* registering a new target id;
* adding a new `DAUx*` `Info.plist` key;
* adding a new category slug, framework, renderer or presentation mode **only if** v1
  readers treating it as an error (§3.6, §3.9) is acceptable for that change — in practice
  this means new enum values ship together with a `formatVersion` bump unless the key is
  itself new and optional.

It MUST be bumped to `2` when any of these happens:

* an existing key changes JSON type, meaning, or unit;
* a previously optional key becomes required, or a default changes;
* a length, count or range limit from §10 is **raised** — v1 readers reject the larger
  value, so a raise is breaking;
* the bundle directory layout changes such that a v1 reader would resolve a path wrongly;
* an existing key is removed or renamed.

Removing a key that was already optional-and-ignorable, or narrowing a limit, is still a
bump: readers that relied on it break.

### 9.3 A v1 reader meets a v2 manifest

1. Parse `format`. If it is not `"DAUx Audio Extension"`, reject with `DAUX-M005`.
2. Parse `formatVersion`. If it is `> 1`, the reader MUST NOT attempt to interpret the rest
   of the document. It MUST reject the bundle with `DAUX-M006`.
3. Before giving up it MUST make one further pass to read the stable prologue (§3.3) —
   `abiVersion`, `plugin.id`, `plugin.name`, `plugin.version` — so the error can say
   *"EQUZX 2.0.0 (studio.futureboard.equzx) uses AXT format 2; this host supports 1"*.
   If even the prologue is unreadable, the message degrades to the file path.
4. It MUST NOT guess, MUST NOT fall back to "treat as v1", and MUST NOT load the binary.
   A v2 bundle may use a directory layout v1 cannot resolve; loading the wrong library is
   worse than not loading one.
5. `formatVersion` `0`, negative, fractional, or not a JSON number is `DAUX-M006` /
   `DAUX-M008`, never coerced.

A v2 reader meeting a v1 manifest MUST apply the v1 defaults documented here for every key
v2 added, and MUST accept the bundle. Backwards compatibility is mandatory in that
direction; the format is append-only in spirit exactly like the ABI structures of
abi-v1 §1.

---

## 10. Security limits a conforming parser MUST enforce

A `.axt` is a directory a user obtained from the internet. The metadata parser is the first
code to touch it, it runs on the main thread during scanning, and it MUST survive
deliberate abuse. All of the following are **MUST**, and every violation produces a
`BundleError` / `ValidationIssue` — never a panic, never a hang, never an unbounded
allocation.

### 10.1 Size and shape

| Limit                                        | Value                       | Code        |
| -------------------------------------------- | --------------------------- | ----------- |
| Metadata file size (`manifest.json`, `Info.plist`) | 4 MiB               | `DAUX-M002` |
| Any single string value (hard cap)           | 4 KiB                       | `DAUX-M009` |
| Per-field string limits                      | §3.2, §3.7, §3.10, §3.11    | `DAUX-M009` |
| `targets` entries                            | 256                         | `DAUX-M012` |
| `dependencies` entries                       | 256                         | `DAUX-M008` |
| `capabilities` keys                          | 256                         | `DAUX-M008` |
| `features` entries                           | 32                          | `DAUX-M008` |
| Keys in any one object / dict                | 1024                        | `DAUX-M008` |
| Array elements in any one array              | 1024                        | `DAUX-M008` |
| Nesting depth (JSON or plist)                | 16                          | `DAUX-M018` |

The file size limit MUST be applied to the *read*, not after it: the reader checks the
directory entry's length and refuses to read further, so a 4 GiB `manifest.json` costs one
`stat`. Reading into a pre-sized buffer with an explicit cap is the only acceptable
pattern; `read_to_string` on an unbounded file is a bug.

### 10.2 Encoding

* The file MUST be valid UTF-8. Invalid bytes reject the file with `DAUX-M003`. Lossy
  conversion is **forbidden here**, unlike the fixed ABI text buffers of abi-v1 §2.1 —
  an ABI call cannot fail mid-flight and must degrade, whereas a file read can and MUST
  fail cleanly.
* A leading UTF-8 BOM MAY be skipped. UTF-16/UTF-32 BOMs MUST be rejected (`DAUX-M003`).
* Strings MUST NOT contain unpaired surrogates, `U+0000`, or the control characters listed
  in §3.2. Non-characters and unassigned code points are permitted.
* Binary plists MUST be validated for internal offset consistency before any object is
  materialised; a truncated or self-referential object table is `DAUX-M004`.

### 10.3 Parsing discipline

* **Duplicate keys in one JSON object or plist dict MUST be rejected** (`DAUX-M019`).
  "Last one wins" is a parser-differential bug: the scanner and the validator would
  disagree about what the bundle claims.
* Integer-typed keys MUST be parsed as exact integers. A reader MUST NOT route
  `formatVersion` or `abiVersion` through `f64`, and MUST NOT use `as` casts that truncate.
  `1.0`, `1e0`, `"1"` and `0x1` are all `DAUX-M008`.
* Numbers outside a documented range are `DAUX-M016`; NaN and infinities are not JSON and
  are `DAUX-M004`.
* Parsing MUST be O(n) in input size and MUST NOT recurse deeper than the §10.1 depth
  limit — either iteratively, or with an explicit depth counter checked before descending.
* The parser MUST NOT resolve references, includes, external entities, or URLs. There is no
  `$ref` in this format and none will be added. The XML plist reader MUST have DTD and
  external-entity processing disabled.
* Every slice, index and arithmetic operation on parsed data MUST be bounds- and
  overflow-checked. `unwrap`, `expect`, `panic!`, slicing by unvalidated index, and
  `unreachable!` on parsed input are forbidden in this code path; the crate SHOULD keep a
  test that feeds truncated, oversized, deeply nested, duplicate-keyed and non-UTF-8
  inputs to every entry point and asserts an `Err` for each.

### 10.4 Paths

Every string this format allows to influence a filesystem operation — `resources.dir`,
`resources.libraryDir`, `dependencies` entries, `CFBundleExecutable`, and every logical
path passed to `ResourceDir` — MUST be validated before use:

* no `..` component, no absolute path, no drive letter (`C:`), no UNC prefix (`\\`),
  no leading `/` or `\`;
* no NUL or control characters; no trailing space or dot (Windows silently strips them);
* not a Windows reserved device name in any case, with or without extension;
* directory and dependency names are a single segment; logical resource paths use `/` as
  the only separator;
* after resolution, the canonicalised result MUST still be inside the bundle directory —
  this is what catches symlinks pointing at `/etc/shadow` or `C:\Windows\System32`.

Violations are `DAUX-M055` / `BundleError::PathEscape`. The check is on the *canonicalised*
path, not the textual one; textual checks alone are defeated by symlinks.

### 10.5 Issue codes

`ValidationIssue::code` values are stable strings; tooling and tests may match on them.

| Code         | Severity | Meaning                                                        |
| ------------ | -------- | -------------------------------------------------------------- |
| `DAUX-M001`  | Error    | no metadata file for the detected layout                        |
| `DAUX-M002`  | Error    | metadata file exceeds 4 MiB                                     |
| `DAUX-M003`  | Error    | not valid UTF-8 / unsupported encoding                          |
| `DAUX-M004`  | Error    | not valid JSON / not a valid plist                              |
| `DAUX-M005`  | Error    | `format` is not `"DAUx Audio Extension"`                        |
| `DAUX-M006`  | Error    | unsupported `formatVersion`                                     |
| `DAUX-M007`  | Error    | required key missing                                            |
| `DAUX-M008`  | Error    | wrong type, or count limit exceeded                             |
| `DAUX-M009`  | Error    | string exceeds its byte limit                                   |
| `DAUX-M010`  | Error    | malformed plug-in id                                            |
| `DAUX-M011`  | Error    | malformed version string                                        |
| `DAUX-M012`  | Error    | `targets` empty or over 256 entries                             |
| `DAUX-M013`  | Error    | malformed or duplicate target id                                |
| `DAUX-M014`  | Error    | unsupported `abiVersion`                                        |
| `DAUX-M015`  | Error    | unknown enum value (category, framework, renderer, presentation) |
| `DAUX-M016`  | Error    | numeric value out of range or bounds inverted                   |
| `DAUX-M017`  | Error    | invalid `resources.dir` / `resources.libraryDir`                |
| `DAUX-M018`  | Error    | nesting depth exceeded                                          |
| `DAUX-M019`  | Error    | duplicate key in one object                                     |
| `DAUX-M050`  | Error    | no binary for a declared target                                 |
| `DAUX-M051`  | Warning  | binary present for an undeclared target                         |
| `DAUX-M052`  | Error    | more than one candidate binary in `Content/{target}`            |
| `DAUX-M053`  | Error    | declared dependency missing                                     |
| `DAUX-M054`  | Warning  | resources directory declared but absent                         |
| `DAUX-M055`  | Error    | path escapes the bundle                                         |
| `DAUX-M056`  | Error    | mixed layout (`manifest.json` and `Info.plist` both present)     |
| `DAUX-M057`  | Error    | bundle directory does not end in `.axt`                         |
| `DAUX-M058`  | Error    | invalid or reserved bundle name                                 |
| `DAUX-M059`  | Error    | Apple and non-Apple targets in one bundle                       |
| `DAUX-M100`  | Error    | manifest/binary **id** mismatch (fatal: not registered)          |
| `DAUX-M101`  | Error    | manifest/binary name mismatch                                   |
| `DAUX-M102`  | Error    | manifest/binary vendor mismatch                                 |
| `DAUX-M103`  | Error    | manifest/binary version mismatch                                |
| `DAUX-M104`  | Warning  | manifest/binary category mismatch                               |
| `DAUX-M105`  | Error    | manifest/binary capability bitset mismatch                      |
| `DAUX-M106`  | Error    | manifest `abiVersion` ≠ entry `abi_version_major`               |
| `DAUX-M107`  | Error    | GUI declaration inconsistent (`graphics` vs `hasGui`)           |
| `DAUX-M108`  | Error    | principal id not exported by the factory (fatal: not registered) |
| `DAUX-M200`  | Error    | `[package.metadata.daux]` missing                               |
| `DAUX-M201`  | Error    | required key missing in `[package.metadata.daux]`               |
| `DAUX-M202`  | Error    | kebab-case and camelCase spellings of one key both present      |
| `DAUX-M203`  | Warning  | generated metadata file found in the source tree                |
| `DAUX-M204`  | Error    | crate does not build a `cdylib`                                 |
| `DAUX-M205`  | Warning  | unknown key in `[package.metadata.daux]`                        |
| `DAUX-M206`  | Warning  | version suffix dropped when normalising `package.version`       |

---

## 11. Conformance checklist

A **bundle** conforms to AXT format v1 when it:

- [ ] is a directory named `{BundleName}.axt` with exactly one layout (§4);
- [ ] contains `manifest.json` (POSIX) or `Contents/Info.plist` (Apple), never both;
- [ ] carries every required key of §3.1/§3.2 or §6, within every limit of §10;
- [ ] declares at least one target and ships an unambiguous binary for each one;
- [ ] agrees with its binary on every row of §8.1;
- [ ] contains no path, dependency name or directory name that escapes the bundle.

A **writer** (`daux build`, `daux bundle`) conforms when it:

- [ ] derives every value from `[package.metadata.daux]` and `[package]`, and nothing else;
- [ ] regenerates the metadata file unconditionally, never merging with a previous one;
- [ ] fails on a missing required key rather than inventing a placeholder;
- [ ] never silently truncates a value that exceeds a §3.2 limit;
- [ ] emits byte-identical output for identical input (§3.13).

A **reader** (`daux-bundle`, `daux-scan`, any host) conforms when it:

- [ ] enforces every limit in §10 before allocating on parsed data;
- [ ] rejects a `formatVersion` it does not implement, after reading the stable prologue;
- [ ] ignores unknown keys, unknown capability names and unregistered target ids;
- [ ] never panics on any input, including truncated, oversized, non-UTF-8, deeply nested
      and duplicate-keyed files;
- [ ] treats the binary's descriptor as authoritative after load, and reports every
      manifest disagreement rather than silently preferring either side.

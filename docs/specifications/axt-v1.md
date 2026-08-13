# AXT Bundle Format — Specification v1

Status: **Stable draft** · Bundle format version: `1` · Extension: `.axt` · Companion: `docs/specifications/abi-v1.md`

This document is the on-disk contract for a **DAUx Audio Extension** (`.axt`). It defines
what a bundle contains, how a host finds and loads it, how resources and dependencies are
resolved, and what a conforming validator checks. It is normative. `crates/daux-bundle` is
the reference implementation of this document; where the two disagree, **this document
wins** and `daux-bundle` is a bug.

`abi-v1.md` governs everything that happens *after* the dynamic library is open. This
document governs everything up to that point, and the parts of the bundle the binary never
sees (layout, manifest, resources, packaging).

Key words **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, **MAY** are used as in RFC 2119.

---

## 1. Scope and design rules

A bundle is **untrusted input**. It arrives as a download, an installer payload, or a
directory a user dragged into place. Everything in this document that looks like paranoia —
bounded field sizes, rejected path components, scoped library search — exists because a
scanner reads thousands of these at start-up, on the user's account, before any signature
or sandbox has vouched for them.

Design rules that shape the rest of the document:

| Rule                                       | Consequence                                                   |
| ------------------------------------------ | ------------------------------------------------------------- |
| Metadata is readable without running code   | A scan MUST be able to complete with zero `dlopen` calls       |
| One bundle, one plug-in identity            | The bundle directory name never determines identity; the id does |
| Platform-native where it matters            | macOS uses Apple bundle conventions, not a DAUx invention      |
| Layout differs, model does not              | Both layouts normalise to one `BundleMetadata` (§8)            |
| Nothing global                              | Loading a bundle MUST NOT mutate process-wide state (§11)      |
| Forward compatible by ignoring              | Unknown keys and unknown files are ignored, never fatal        |

A v1 bundle is a **directory**, not an archive. A future format version MAY define a
single-file container; v1 loaders MUST reject a regular file named `*.axt`.

---

## 2. Bundle identity

```text
<BundleName>.axt/
```

* The extension MUST be `.axt`. Comparison MUST be ASCII case-insensitive (`.AXT` is the
  same extension) because Windows and macOS filesystems are case-insensitive by default.
* `<BundleName>` SHOULD be the plug-in's display name reduced to `[A-Za-z0-9 ._-]`, MUST be
  valid UTF-8, MUST be 1..=64 bytes, and MUST NOT begin or end with `.` or a space.
* `<BundleName>` carries **no semantics**. Plug-in identity is `plugin.id` (§7) and, at
  runtime, `DauxPluginDescriptorV1::id` (abi-v1 §6). Two bundles with the same name and
  different ids are two different plug-ins; two bundles with different names and the same
  id are the same plug-in, and a host SHOULD report the duplicate.
* Directory and file names inside the bundle are **case-sensitive** and MUST be spelled
  exactly as this document spells them. A loader MUST NOT retry a failed lookup with a
  different case: doing so produces bundles that work on Windows and macOS and fail on
  Linux, which is the single most common cross-platform packaging bug.
* Unknown extra files and directories at any level MUST be ignored by loaders. `daux
  validate` MAY report them at `Info` severity.

Conventional install locations — a host SHOULD scan these and MUST allow the user to add
more:

| Platform | System-wide                                | Per-user                                    | Override           |
| -------- | ------------------------------------------ | ------------------------------------------- | ------------------ |
| Windows  | `%CommonProgramFiles%\DAUx\AXT`            | `%LOCALAPPDATA%\Programs\DAUx\AXT`          | `DAUX_AXT_PATH`    |
| Linux    | `/usr/lib/daux/axt`, `/usr/local/lib/daux/axt` | `$XDG_DATA_HOME/daux/axt`, `~/.local/lib/daux/axt` | `DAUX_AXT_PATH` |
| macOS    | `/Library/Audio/Plug-Ins/AXT`              | `~/Library/Audio/Plug-Ins/AXT`              | `DAUX_AXT_PATH`    |

`DAUX_AXT_PATH` is a platform-separated list (`;` on Windows, `:` elsewhere). Search paths
are searched in order; the first bundle with a given plug-in id wins and later duplicates
MUST be reported, not silently dropped.

---

## 3. Target identifiers

A **target identifier** names one binary slice inside a bundle. It is the only vocabulary
that appears in bundle paths and in `targets`.

```text
windows-x86_64
windows-aarch64
linux-x86_64
linux-aarch64
macos-x86_64
macos-arm64
macos-universal
```

Identifiers are lowercase ASCII, `-` separated, of the form `{os}-{arch}`. The set above is
**closed** in format version 1: a loader MUST reject an unknown identifier in `targets`
rather than guess, and MUST ignore an unknown-named directory under `Content/` or
`Library/`.

### 3.1 Mapping to Rust target triples

| Target id         | Rust triples (all equivalent for bundling)                            | Binary format |
| ----------------- | --------------------------------------------------------------------- | ------------- |
| `windows-x86_64`  | `x86_64-pc-windows-msvc`, `x86_64-pc-windows-gnu`, `x86_64-pc-windows-gnullvm` | PE32+ |
| `windows-aarch64` | `aarch64-pc-windows-msvc`, `aarch64-pc-windows-gnullvm`               | PE32+         |
| `linux-x86_64`    | `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`               | ELF64         |
| `linux-aarch64`   | `aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`             | ELF64         |
| `macos-x86_64`    | `x86_64-apple-darwin`                                                 | Mach-O 64     |
| `macos-arm64`     | `aarch64-apple-darwin`                                                | Mach-O 64     |
| `macos-universal` | `x86_64-apple-darwin` **and** `aarch64-apple-darwin`, merged with `lipo` | Mach-O fat |

Notes that are deliberate, not oversights:

* `macos-arm64` spells the architecture the way Apple's own tools do (`arm64`), while
  `linux-aarch64` uses the ELF/Rust spelling. Matching each platform's native vocabulary is
  worth the asymmetry; `TargetId::from_rust_triple("aarch64-apple-darwin")` MUST return
  `macos-arm64`, never `macos-aarch64`.
* The libc flavour (`gnu` vs `musl`) is **not** part of the target id. A bundle that ships a
  musl-linked `linux-x86_64` binary and a bundle that ships a glibc-linked one are
  indistinguishable at the format level. Publishers targeting broad Linux compatibility
  SHOULD link against the oldest practical glibc rather than encode the flavour in a path.
* `macos-universal` is a single fat binary, not two directories. A bundle MUST NOT list
  `macos-universal` together with `macos-x86_64` or `macos-arm64`.

### 3.2 Dynamic library naming

| Target family | Location                          | File name              | Notes                                   |
| ------------- | --------------------------------- | ---------------------- | --------------------------------------- |
| `windows-*`   | `Content/{target}/`               | `<BundleName>.dll`     | No `lib` prefix. Cargo already omits it. |
| `linux-*`     | `Content/{target}/`               | `<BundleName>.so`      | **No `lib` prefix** — the bundler renames Cargo's `lib<crate>.so`. |
| `macos-*`     | `Contents/MacOS/`                 | `<BundleName>`         | No extension at all; `CFBundleExecutable` MUST match. |

* The stem MUST equal `<BundleName>` exactly, including case.
* A loader MUST accept `lib<BundleName>.so` on Linux as a tolerance fallback when
  `<BundleName>.so` is absent; `daux validate` MUST report this at `Warning` severity.
  No equivalent fallback exists on Windows or macOS.
* The macOS binary MUST be a Mach-O `MH_DYLIB` or `MH_BUNDLE` loadable with `dlopen`. It
  MUST NOT be `MH_EXECUTE`.
* Every target binary MUST export `daux_plugin_entry_v1` (abi-v1 §4) and MUST NOT export any
  other symbol beginning with `daux_`. Rust plug-ins achieve this with
  `crate-type = ["cdylib"]`.
* `TargetId::dylib_extension()` returns `"dll"`, `"so"` and `""` respectively.

---

## 4. Layout detection

Two layouts exist. A bundle uses exactly one.

```text
1. The path MUST be a directory whose name ends in ".axt" (ASCII case-insensitive).
2. If <bundle>/Contents/Info.plist exists            → Apple layout   (§6)
3. else if <bundle>/manifest.json exists             → POSIX layout   (§5)
4. else                                              → error: not a bundle
```

Detection MUST be platform-independent: inspecting a macOS bundle on Windows MUST yield
`BundleLayout::Apple`, so that `daux inspect` and cross-compiling build machines see the
same thing the target platform will.

A bundle MUST NOT contain both `Contents/Info.plist` and `manifest.json`. A loader that
encounters both MUST resolve to the Apple layout (rule 2 above, deterministically) and
`daux validate` MUST report `axt.layout.ambiguous` at `Error` severity.

`macos-*` targets MUST NOT appear in a POSIX-layout bundle, and `windows-*` / `linux-*`
targets MUST NOT appear in an Apple-layout bundle. One `.axt` MAY serve Windows and Linux
simultaneously; macOS always ships its own bundle.

---

## 5. POSIX layout (Windows, Linux)

```text
Plugin.axt/
├─ Content/
│  └─ {target}/
│     └─ Plugin.{dll|so}
│
├─ Resources/
│  └─ *
│
├─ Library/
│  └─ {target}/
│     └─ *
│
└─ manifest.json
```

| Entry                          | Required | Contents                                                    |
| ------------------------------ | -------- | ----------------------------------------------------------- |
| `manifest.json`                | MUST     | Bundle metadata, §7                                          |
| `Content/{target}/`            | MUST     | Exactly one plug-in binary per listed target, §3.2           |
| `Resources/`                   | MAY      | Logical resource root, §10                                   |
| `Library/{target}/`            | MAY      | Bundled dynamic dependencies for that target, §11            |
| `Signature/`                   | MAY      | Optional integrity manifest and signature, §13               |

`Content` is singular and `Library` is singular; `Resources` is plural. This is unfortunate
and it is fixed.

Every target listed in `manifest.json` MUST have a `Content/{target}/` directory containing
its binary. A `Content/{target}/` directory whose target is not listed MUST be ignored by
loaders and reported by `daux validate`.

`Content/{target}/` MUST contain exactly one plug-in binary. Additional files in that
directory (debug symbols, import libraries) MUST NOT be loaded; publishers SHOULD NOT ship
them. Side-by-side dependencies belong in `Library/{target}/`, not next to the binary.

### 5.1 Worked example — Windows

```text
EQUZX.axt/
├─ Content/
│  └─ windows-x86_64/
│     └─ EQUZX.dll
│
├─ Resources/
│  ├─ Shaders/
│  ├─ Images/
│  ├─ Presets/
│  └─ Data/
│
├─ Library/
│  └─ windows-x86_64/
│     ├─ dependency.dll
│     └─ optional-runtime.dll
│
└─ manifest.json
```

### 5.2 Worked example — Linux

```text
EQUZX.axt/
├─ Content/
│  └─ linux-x86_64/
│     └─ EQUZX.so
│
├─ Resources/
├─ Library/
│  └─ linux-x86_64/
│     └─ libdependency.so
│
└─ manifest.json
```

Note the asymmetry inside `Library/`: the *plug-in* binary is `EQUZX.so` (§3.2) while
*dependencies* keep whatever `soname` their build produced, `libdependency.so` included.
Dependencies are resolved by the dynamic linker under their own names; the bundler MUST NOT
rename them.

### 5.3 Worked example — one bundle, two platforms

```text
EQUZX.axt/
├─ Content/
│  ├─ windows-x86_64/EQUZX.dll
│  ├─ windows-aarch64/EQUZX.dll
│  ├─ linux-x86_64/EQUZX.so
│  └─ linux-aarch64/EQUZX.so
├─ Library/
│  ├─ windows-x86_64/dependency.dll
│  └─ linux-x86_64/libdependency.so
├─ Resources/
│  └─ Shaders/spectrum.wgsl
└─ manifest.json
```

`Library/{target}/` is per target and MAY be absent for targets with no bundled
dependencies, as `windows-aarch64` and `linux-aarch64` are here.

---

## 6. Apple layout (macOS)

macOS uses native Apple bundle conventions. The POSIX layout MUST NOT be used on macOS: it
breaks `codesign`, notarization, Gatekeeper, and every Finder and installer expectation.

```text
Plugin.axt/
└─ Contents/
   ├─ Info.plist
   ├─ MacOS/
   │  └─ Plugin
   ├─ Frameworks/
   │  └─ *
   └─ Resources/
      └─ *
```

| Entry                       | Required | Corresponds to (POSIX)      |
| --------------------------- | -------- | --------------------------- |
| `Contents/Info.plist`       | MUST     | `manifest.json`             |
| `Contents/MacOS/<BundleName>` | MUST   | `Content/{target}/<bin>`    |
| `Contents/Resources/`       | MAY      | `Resources/`                |
| `Contents/Frameworks/`      | MAY      | `Library/{target}/`         |
| `Contents/_CodeSignature/`  | MAY      | — (Apple, §13.3)            |
| `Contents/Signature/`       | MAY      | `Signature/` (§13)          |

There is **no** `{target}` level on macOS. Architecture selection is the Mach-O loader's
job: one file in `Contents/MacOS/` serves `macos-x86_64`, `macos-arm64` or
`macos-universal`, and `DAUxTargets` declares which. Universal distribution SHOULD be
preferred; a bundle listing `macos-universal` whose Mach-O is thin is a validation error.

### 6.1 Worked example — macOS

```text
EQUZX.axt/
└─ Contents/
   ├─ Info.plist
   │
   ├─ MacOS/
   │  └─ EQUZX
   │
   ├─ Frameworks/
   │  ├─ SomeDependency.framework/
   │  └─ libSomething.dylib
   │
   └─ Resources/
      ├─ Shaders/
      ├─ Images/
      └─ Presets/
```

`Contents/Frameworks/` MAY contain `.framework` directories, `.dylib` files, or both. Use
it for dependencies; a macOS bundle MUST NOT contain a top-level `Library/` directory.

---

## 7. `manifest.json`

UTF-8 JSON, no BOM, no comments, no trailing commas. Object keys are `lowerCamelCase`.

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

### 7.1 Field reference

| Key                    | Type            | Required | Meaning                                                     |
| ---------------------- | --------------- | -------- | ----------------------------------------------------------- |
| `format`               | string          | MUST     | Exactly `"DAUx Audio Extension"`; a cheap sanity gate        |
| `formatVersion`        | integer         | MUST     | Bundle format version, `1` in this document (§12)            |
| `abiVersion`           | integer         | MUST     | DAUx ABI **major** version the binaries were built against   |
| `plugin.id`            | string          | MUST     | Reverse-DNS, ASCII, ≤127 bytes; permanent (abi-v1 §14)       |
| `plugin.name`          | string          | MUST     | Display name                                                 |
| `plugin.vendor`        | string          | MUST     | Publisher display name                                       |
| `plugin.version`       | string          | MUST     | `major.minor.patch[.build]`, decimal, no `v` prefix          |
| `plugin.description`   | string          | SHOULD   | One line; `""` when absent                                   |
| `plugin.url`           | string          | MAY      | Product page                                                 |
| `plugin.supportUrl`    | string          | MAY      | Support page                                                 |
| `plugin.copyright`     | string          | MAY      | Copyright notice                                             |
| `plugin.license`       | string          | MAY      | SPDX identifier where applicable                             |
| `plugin.category`      | string          | SHOULD   | `effect`\|`instrument`\|`midiEffect`\|`analyzer`\|`generator`\|`utility`\|`unknown` |
| `targets`              | array\<string\> | MUST     | ≥1 target id (§3), no duplicates                             |
| `capabilities`         | object          | SHOULD   | Booleans, §7.2; omitted keys are `false`                     |
| `graphics`             | object          | MAY      | §7.3; absence means "no editor"                              |
| `dependencies`         | array\<string\> | MAY      | §7.4                                                          |
| `resources`            | object          | MAY      | §7.5                                                          |
| `plugins`              | array\<object\> | MAY      | §7.6, multi-plug-in bundles                                   |
| `stateSchemaVersion`   | integer         | MAY      | Scanner hint; the descriptor is authoritative (§8)           |
| `sdk`                  | object          | MAY      | `{ "name": …, "version": … }`, diagnostics only              |
| `minHost`              | object          | MAY      | `{ "dauxAbiMinor": 0 }`, advisory                             |

Unknown keys MUST be ignored. This is the only forward-compatibility mechanism the manifest
has, so a loader MUST NOT reject a document because it does not recognise a key.

### 7.2 Capability keys

Each key maps to one `DAUX_CAP_*` bit (abi-v1 §6.2). The manifest form exists so a scanner
can filter — "show me instruments" — without opening a single library.

| Manifest key               | ABI bit                         | Manifest key            | ABI bit                       |
| -------------------------- | ------------------------------- | ----------------------- | ----------------------------- |
| `audioEffect`              | `DAUX_CAP_AUDIO_EFFECT`         | `hasGui`                | `DAUX_CAP_HAS_GUI`            |
| `instrument`               | `DAUX_CAP_INSTRUMENT`           | `requiresGui`           | `DAUX_CAP_REQUIRES_GUI`       |
| `midiEffect`               | `DAUX_CAP_MIDI_EFFECT`          | `sharedTextureGui`      | `DAUX_CAP_SHARED_TEXTURE_GUI` |
| `analyzer`                 | `DAUX_CAP_ANALYZER`             | `offlineRender`         | `DAUX_CAP_OFFLINE_RENDER`     |
| `midiInput`                | `DAUX_CAP_MIDI_INPUT`           | `hardRealtime`          | `DAUX_CAP_HARD_REALTIME`      |
| `midiOutput`               | `DAUX_CAP_MIDI_OUTPUT`          | `sandboxSafe`           | `DAUX_CAP_SANDBOX_SAFE`       |
| `midi2`                    | `DAUX_CAP_MIDI2`                | `stereoOnly`            | `DAUX_CAP_STEREO_ONLY`        |
| `sidechain`                | `DAUX_CAP_SIDECHAIN`            | `latencyDynamic`        | `DAUX_CAP_LATENCY_DYNAMIC`    |
| `dynamicBuses`             | `DAUX_CAP_DYNAMIC_BUSES`        | `tailInfinite`          | `DAUX_CAP_TAIL_INFINITE`      |
| `sampleAccurateAutomation` | `DAUX_CAP_SAMPLE_ACCURATE_AUTO` | `noteExpression`        | `DAUX_CAP_NOTE_EXPRESSION`    |

An unknown capability key MUST be ignored, not rejected: a v2 SDK will add bits.

### 7.3 `graphics`

| Key             | Type    | Values                                              |
| --------------- | ------- | --------------------------------------------------- |
| `enabled`       | bool    | `false` ⇒ headless; the object MAY then be omitted   |
| `framework`     | string  | `gpui` \| `egui` \| `custom`                         |
| `renderer`      | string  | `wgpu` \| `opengl` \| `software`                     |
| `resizable`     | bool    | Editor may be resized by the host                    |
| `width`         | integer | Preferred logical width, > 0                         |
| `height`        | integer | Preferred logical height, > 0                        |
| `aspectRatio`   | number  | Locked aspect ratio, > 0; omit for free resize       |

The declaration is a **hint for the host UI** (window pre-sizing, GPU capability filtering).
The plug-in's `daux.gui/1` extension (abi-v1 §11.4) is authoritative once loaded.

### 7.4 `dependencies`

```json
"dependencies": ["dependency.dll", "optional-runtime.dll"]
```

Each entry is a **single file name**, resolved inside `Library/{target}/` (POSIX) or
`Contents/Frameworks/` (Apple). An entry MUST NOT contain `/`, `\`, `.` or `..` as a path
component, MUST NOT be absolute, and is subject to §10.2 in full. The list is documentation
and a validation aid — the dynamic linker, not the manifest, performs the actual resolution
(§11). The list MAY be incomplete for transitive dependencies; `daux validate` treats a
listed-but-missing file as an error and an unlisted-but-present file as informational.

### 7.5 `resources`

```json
"resources": {
  "required": ["Shaders/spectrum.wgsl"],
  "optional": ["Presets/Default.dauxpreset"]
}
```

Both arrays hold **logical paths** (§10). `daux validate` MUST verify that every `required`
entry exists and that every entry in either array is a legal logical path. Declaring
resources is optional; declaring them badly is an error.

### 7.6 `plugins` — multi-plug-in bundles

One binary MAY export several plug-ins (abi-v1 §5). `plugin` describes the bundle's
**primary** plug-in; the optional `plugins` array lets a scanner list the rest without
loading code:

```json
"plugins": [
  { "id": "studio.futureboard.equzx",       "name": "EQUZX",       "category": "effect" },
  { "id": "studio.futureboard.equzx.mini",  "name": "EQUZX Mini",  "category": "effect" }
]
```

When present, `plugins` MUST contain `plugin.id`. The factory's descriptor enumeration is
authoritative; a mismatch is reported by §15 and never changes runtime behaviour.

### 7.7 Bounds

A parser MUST enforce these before allocating, and MUST fail with a diagnostic rather than
panic or allocate proportionally to a hostile length field:

| Limit                                  | Value      |
| -------------------------------------- | ---------- |
| `manifest.json` file size              | ≤ 4 MiB    |
| Any string value                       | ≤ 4 KiB    |
| `plugin.id` length                     | ≤ 127 bytes |
| `targets` entries                      | ≤ 256      |
| `dependencies` entries                 | ≤ 1024     |
| `resources.required` + `.optional`     | ≤ 4096 combined |
| `plugins` entries                      | ≤ 1024     |
| JSON nesting depth                     | ≤ 32       |
| Logical path length                    | ≤ 1024 bytes (matches `DAUX_PATH_SIZE`) |
| Single path component                  | ≤ 255 bytes |

Exceeding any limit MUST produce a `BundleError`, never a panic and never an unbounded
allocation.

---

## 8. `Info.plist` and the normalised metadata model

On macOS `manifest.json` is replaced by `Contents/Info.plist`. The two carry the same
information; both normalise to one `BundleMetadata`.

`Info.plist` MUST be an XML property list (`<!DOCTYPE plist ...>`, `version="1.0"`, UTF-8).
Loaders MAY additionally accept the binary plist encoding; publishers MUST ship XML so that
non-Apple tooling can read the bundle without a binary-plist parser. `plutil -convert xml1`
converts an existing file. The §7.7 bounds apply unchanged, with the file-size limit
applying to `Info.plist`.

### 8.1 Key mapping

| Normalised field     | `manifest.json`        | `Info.plist`                                       |
| -------------------- | ---------------------- | -------------------------------------------------- |
| `id`                 | `plugin.id`            | `CFBundleIdentifier`                                |
| `name`               | `plugin.name`          | `CFBundleDisplayName`, else `CFBundleName`          |
| `vendor`             | `plugin.vendor`        | `DAUxVendor`                                        |
| `version`            | `plugin.version`       | `CFBundleShortVersionString`                        |
| *(build)*            | `plugin.version` build | `CFBundleVersion`                                   |
| `description`        | `plugin.description`   | `DAUxDescription`                                   |
| `category`           | `plugin.category`      | `DAUxPluginType`                                    |
| `format_version`     | `formatVersion`        | `DAUxFormatVersion` (integer)                       |
| `abi_version`        | `abiVersion`           | `DAUxAbiVersion` (integer)                          |
| `targets`            | `targets`              | `DAUxTargets` (array of strings)                    |
| `capabilities`       | `capabilities`         | `DAUxCapabilities` (array of the §7.2 key names)    |
| `graphics`           | `graphics`             | `DAUxGraphics` (dict, same keys as §7.3)            |
| *(binary name)*      | derived from `<BundleName>` | `CFBundleExecutable`                          |
| *(entry symbol)*     | implicit               | `DAUxEntrypoint`                                    |
| *(dependencies)*     | `dependencies`         | `DAUxDependencies` (array of strings)               |
| *(resources)*        | `resources`            | `DAUxResources` (dict with `required`/`optional`)   |
| *(state hint)*       | `stateSchemaVersion`   | `DAUxStateSchemaVersion` (integer)                  |
| *(package type)*     | —                      | `CFBundlePackageType` = `BNDL`                      |

`Info.plist` boolean values use `<true/>`/`<false/>`; `DAUxCapabilities` is an *array of the
enabled capability names* rather than a dictionary of booleans, because arrays survive
`plutil` round-trips and diff cleanly.

Additional requirements on the Apple side:

* `CFBundlePackageType` MUST be `BNDL`. It MUST NOT be `APPL`.
* `CFBundleExecutable` MUST name the file in `Contents/MacOS/` exactly, and that name MUST
  equal `<BundleName>` (§3.2).
* `CFBundleIdentifier` MUST equal the plug-in id. It is also what `codesign` and Gatekeeper
  key on, so it MUST NOT be changed for cosmetic reasons (abi-v1 §14).
* `CFBundleName` is limited to 15 characters by Apple convention; longer display names MUST
  use `CFBundleDisplayName`.
* `CFBundleInfoDictionaryVersion` SHOULD be `6.0`. `LSMinimumSystemVersion` MAY be present.
* `DAUxEntrypoint` MAY be omitted; when omitted it defaults to `daux_plugin_entry_v1`. When
  present it MUST be `daux_plugin_entry_v1` in format version 1 — the key exists so that a
  future ABI generation can be declared without a format break.

### 8.2 Worked example — `Contents/Info.plist`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>            <string>studio.futureboard.equzx</string>
  <key>CFBundleName</key>                  <string>EQUZX</string>
  <key>CFBundleDisplayName</key>           <string>EQUZX</string>
  <key>CFBundleExecutable</key>            <string>EQUZX</string>
  <key>CFBundlePackageType</key>           <string>BNDL</string>
  <key>CFBundleShortVersionString</key>    <string>1.0.0</string>
  <key>CFBundleVersion</key>               <string>1.0.0.0</string>
  <key>CFBundleInfoDictionaryVersion</key> <string>6.0</string>

  <key>DAUxFormatVersion</key>             <integer>1</integer>
  <key>DAUxAbiVersion</key>                <integer>1</integer>
  <key>DAUxPluginType</key>                <string>effect</string>
  <key>DAUxVendor</key>                    <string>Futureboard Studio</string>
  <key>DAUxDescription</key>               <string>Dynamic equalizer and spectral processor</string>
  <key>DAUxEntrypoint</key>                <string>daux_plugin_entry_v1</string>

  <key>DAUxTargets</key>
  <array>
    <string>macos-universal</string>
  </array>

  <key>DAUxCapabilities</key>
  <array>
    <string>audioEffect</string>
    <string>midiInput</string>
    <string>sidechain</string>
    <string>dynamicBuses</string>
    <string>sampleAccurateAutomation</string>
    <string>hasGui</string>
  </array>

  <key>DAUxGraphics</key>
  <dict>
    <key>enabled</key>    <true/>
    <key>framework</key>  <string>gpui</string>
    <key>renderer</key>   <string>wgpu</string>
    <key>resizable</key>  <true/>
  </dict>
</dict>
</plist>
```

### 8.3 Which side is authoritative

The rule is one sentence: **the manifest is authoritative for what the scanner needs before
executing code; the binary descriptor is authoritative for everything the plug-in reports at
runtime.**

| Field                                        | Authority          | Rationale                                                |
| -------------------------------------------- | ------------------ | -------------------------------------------------------- |
| Layout, target list, binary location          | Manifest           | Needed to decide *whether and what* to open               |
| `formatVersion`                               | Manifest           | Describes the directory, not the code                     |
| Entry symbol name                             | Manifest           | Needed before the first symbol lookup                     |
| Dependency list, resource declarations        | Manifest           | Packaging facts; the binary cannot see them               |
| Graphics framework / renderer / preferred size | Manifest (pre-load), plug-in (post-load) | Host pre-sizes windows, then `daux.gui/1` corrects it |
| `abiVersion`                                  | **Binary**         | `DauxPluginEntryV1` is checked at load; abi-v1 §3 governs |
| Plug-in `id`                                  | **Binary**         | Identity is the descriptor's; the manifest merely advertises it |
| `name`, `vendor`, `version`, `description`, `url`, `copyright`, `license` | **Binary** | The plug-in reports them in `DauxPluginDescriptorV1` |
| `category`, `capabilities`, sample formats    | **Binary**         | The processor decides what it can actually do             |
| `stateSchemaVersion`                          | **Binary**         | abi-v1 §12 owns state compatibility                       |
| Parameter, bus and port ids                   | **Binary**         | Extensions only; never in the manifest                    |

Disagreement handling:

1. `plugin.id` not matching any descriptor id MUST be an `Error` from `daux validate` and
   SHOULD cause a host to refuse the bundle: identity confusion corrupts scan caches and
   saved projects.
2. `abiVersion` disagreeing with the binary MUST be a validation `Error`. At load time the
   binary wins and abi-v1 §3 rejection rules apply unchanged.
3. Any other disagreement is a `Warning`. The host MUST use the binary's value once loaded
   and MUST NOT silently rewrite the bundle.
4. A host MUST NOT skip binary validation because the manifest looked correct. The manifest
   is an index, never a substitute.

The generation of `manifest.json` and `Info.plist` from `[package.metadata.daux]` is
specified in `docs/specifications/manifest-v1.md`; developers do not hand-write either file.

---

## 9. Host loading flow

Normative order. Steps 1–7 execute **no plug-in code**; step 8 is the first that can run
anything the bundle author wrote, and even then only static initialisers (abi-v1 §4).

```text
discover  <Name>.axt                                  §2
    ↓
detect layout                                         §4      (no code)
    ↓
read manifest.json | Contents/Info.plist               §7, §8  (no code)
    ↓
validate metadata and bundle format version            §12     (no code)
    ↓
select target for the host platform                    §3      (no code)
    ↓
locate the target binary                               §5, §6  (no code)
    ↓
configure scoped dependency resolution                 §11     ← before the open
    ↓
open the dynamic library                               §11
    ↓
resolve daux_plugin_entry_v1                           abi-v1 §4
    ↓
validate magic / abi_version_major / size              abi-v1 §3
    ↓
create_factory(host)                                   abi-v1 §4
    ↓
enumerate descriptors                                  abi-v1 §5
    ↓
create_plugin(id) → init → activate                    abi-v1 §7
```

Normative requirements:

1. **Discovery MUST NOT load code.** A directory scan that is only cataloguing MUST stop
   before step 8. Scanners SHOULD cache the result keyed on a fingerprint of the manifest
   path, size and modification time, and MUST re-scan when it changes.
2. Target selection MUST match the **host process** architecture, not the machine's. A
   32-bit-on-64-bit or x86_64-on-arm64 emulated host MUST select the target it can actually
   load. Selection order on macOS is `macos-universal` first, then the exact architecture.
3. If no listed target matches the host, the load MUST fail with `NoBinaryForTarget`. This
   is a normal outcome for a cross-platform bundle and MUST NOT be reported as corruption.
4. Dependency search configuration (§11) MUST be established **before** the library is
   opened. Configuring it afterwards is too late: the loader resolves the import table
   during the open call.
5. Failure at any step MUST unwind the completed steps in reverse: destroy instances,
   `destroy_factory`, then unload the library, then release the dependency search
   configuration.
6. The library MUST remain loaded while any factory, instance, extension table or editor
   derived from it exists (abi-v1 §16.1). An implementation SHOULD make this structural —
   a refcounted module handle held by every derived object — rather than a documented rule.
7. Steps 8–13 are `[main-thread]`. No step of this flow is `[audio-thread]`, and none of it
   may be triggered from `process`.
8. A bundle MUST NOT be trusted to be internally consistent between two reads. A host that
   validated a bundle at scan time and loads it later MAY find it changed; validation
   results are advisory, and the abi-v1 §3 checks at load time are not optional.

---

## 10. Resource resolution

### 10.1 The logical namespace

Plug-in code never learns where the bundle lives:

```rust
ctx.resources().read("Shaders/spectrum.wgsl")?;
ctx.resources().read_to_string("Presets/Default.json")?;
```

A **logical path** is resolved against the layout's resource root:

| Layout | Physical resource root         |
| ------ | ------------------------------ |
| POSIX  | `<bundle>/Resources/`          |
| Apple  | `<bundle>/Contents/Resources/` |

Rules:

* A logical path MUST be relative, UTF-8, and MUST use `/` as its only separator, on every
  platform including Windows. The loader converts `/` to the platform separator by joining
  components; it MUST NOT hand the raw string to the OS.
* Matching MUST be treated as case-sensitive by authors. A loader MUST NOT perform a
  case-insensitive retry (§2).
* Logical paths SHOULD be Unicode NFC. A loader MUST NOT normalise Unicode itself: macOS
  filesystems may store NFD, and silently re-normalising turns a missing file into a
  different missing file.
* Resource reads are blocking file I/O and are therefore **[main-thread]** (or a worker
  thread). A resource read from the audio thread is a real-time violation; `HostResources`
  (crate-contracts, `daux-host-services`) is not exposed through `RtHostServices` for
  exactly this reason.
* `Resources/` MAY be absent. Every read then fails with `NotFound`, which MUST NOT be
  conflated with `PathEscape`.

### 10.2 Traversal rules

A conforming loader MUST reject the following **before touching the filesystem**, returning
`PathEscape` (or `InvalidPath`) and never a partially-resolved path:

| # | Rejected                                                          | Example                       |
| - | ----------------------------------------------------------------- | ----------------------------- |
| 1 | Empty path, or any empty component                                | `""`, `a//b`                  |
| 2 | Leading or trailing `/`                                           | `/Shaders/a.wgsl`, `Shaders/` |
| 3 | A `.` or `..` component                                           | `../../outside`, `a/./b`      |
| 4 | A backslash anywhere                                              | `..\..\outside`               |
| 5 | A colon anywhere                                                  | `C:/x`, `file.txt:stream`     |
| 6 | A Windows reserved device name as a component stem, case-insensitive, with or without extension | `CON`, `nul.txt`, `COM1`, `LPT9`, `AUX`, `PRN` |
| 7 | A component ending in `.` or a space                              | `evil. `, `dir./x`            |
| 8 | Control characters `U+0000..U+001F`, `U+007F`                     | `a\u{0}b`                     |
| 9 | Windows-reserved characters `< > : " \| ? *`                      | `a?b`                         |
| 10 | Any component > 255 bytes, or total path > 1024 bytes            | —                             |
| 11 | A non-UTF-8 byte sequence                                        | —                             |

After the syntactic pass the loader MUST:

12. Join the components onto the resource root using the platform's path API.
13. Canonicalise the result (resolving symlinks, junctions and reparse points) and verify it
    is still inside the canonicalised resource root. A symlink or junction that escapes the
    bundle MUST be rejected, even when every syntactic rule passed.
14. Refuse to open anything that is not a regular file — FIFOs, devices, sockets and block
    devices MUST be rejected, because opening one can block indefinitely.

Note that rules 4–9 are enforced **on every platform**, not only Windows. A bundle that
loads on Linux and fails on Windows because of a `COM1` resource is a broken bundle; making
the rule universal turns a user-visible platform bug into a build-time validation error.

Rejection, not translation, is deliberate for rule 4: a loader that helpfully rewrote `\` to
`/` would let `..\..` slip past a filter written against `/`, which is precisely how
traversal filters are usually defeated.

### 10.3 Why this matters

Once a plug-in's own DLL is loaded, it runs with full host privileges and no path rule can
constrain it. The traversal rules protect the paths where the *host* touches bundle content
on someone else's behalf:

* **The scanner** reads metadata from thousands of bundles at start-up without loading any
  code. A crafted manifest declaring `resources.required = ["../../../etc/shadow"]` must not
  turn a catalogue pass into an arbitrary-read primitive.
* **The sandbox host** (superprompt §42, `daux-protocol`) proxies `HostResources` across a
  trust boundary in the untrusted direction: the sandboxed plug-in asks, the trusted host
  reads. There the plug-in is the attacker and the rules are the boundary.
* **Third-party bundle content.** Resource names frequently come from data inside the bundle
  (a preset naming its shader). That is attacker-controlled input reaching a file open.
* **Uninstall and repackaging.** Tools that walk a bundle must never follow a symlink out of
  it and delete or rewrite something else.

The rules cost one string pass per read and remove an entire class of bug. They are not
optional.

---

## 11. Dependency resolution

Bundled dependencies make a plug-in self-contained. Resolving them MUST NOT make the rest of
the process less predictable.

A loader **MUST NOT** modify any of:

```text
PATH
LD_LIBRARY_PATH
DYLD_LIBRARY_PATH
DYLD_FALLBACK_LIBRARY_PATH
DYLD_INSERT_LIBRARIES
the process working directory
```

These are process-global. Mutating one to load one plug-in changes symbol resolution for the
host and for every other plug-in already loaded, is racy against other threads doing the
same, and is not reliably reversible. The correct mechanisms are all **scoped to a single
load**.

### 11.1 Windows

* The dependency directory is `<bundle>/Library/{target}/`.
* The loader SHOULD call `AddDllDirectory` with the absolute, extended-length
  (`\\?\`-prefixed where needed) path, and MUST open the plug-in with `LoadLibraryExW` and
  the flags `LOAD_LIBRARY_SEARCH_USER_DIRS | LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR |
  LOAD_LIBRARY_SEARCH_SYSTEM32`. These flags govern the transitive import resolution of that
  module, which is exactly the scope wanted.
* A host MUST NOT call `SetDefaultDllDirectories` on behalf of a plug-in: it changes the
  default search strategy for the whole process. Pass the flags per call instead.
* The `AddDllDirectory` cookie MUST be kept for the module's lifetime and released with
  `RemoveDllDirectory` **after** the module is unloaded.
* Bare `LoadLibraryW` MUST NOT be used: its search order includes the application directory
  and `PATH`, which is how a bundle ends up silently binding to a DAW's copy of a library.
* If `Library/{target}/` is absent, no directory is added and the plug-in is expected to be
  self-contained apart from system DLLs.

### 11.2 Linux

* The dependency directory is `<bundle>/Library/{target}/`.
* The plug-in binary MUST carry a `DT_RUNPATH` (new dtags, not the deprecated `DT_RPATH`)
  containing `$ORIGIN` and `$ORIGIN/../../Library/{target}` — from
  `Content/{target}/Plugin.so`, two levels up is the bundle root.
* Because `DT_RUNPATH` is **not** inherited by dependencies, every bundled `.so` in
  `Library/{target}/` MUST itself carry `DT_RUNPATH` of `$ORIGIN` so its own siblings
  resolve.
* Rust emits this with, for example:

  ```text
  -C link-arg=-Wl,-z,origin
  -C link-arg=-Wl,--enable-new-dtags
  -C link-arg=-Wl,-rpath,$ORIGIN:$ORIGIN/../../Library/linux-x86_64
  ```

* The library MUST be opened with `RTLD_NOW | RTLD_LOCAL`.
  `RTLD_NOW` surfaces a missing symbol at load time instead of during a later lazy bind on
  the audio thread. `RTLD_LOCAL` keeps the plug-in's symbols out of the global namespace so
  two plug-ins bundling different versions of the same library do not resolve into each
  other.
* `RTLD_DEEPBIND` MUST NOT be used. It breaks allocator and `pthread` interposition and
  produces mismatched `malloc`/`free` pairs across the boundary.
* `$ORIGIN` expansion is disabled for setuid processes. A host running setuid is out of
  scope; a loader MUST NOT try to work around it by falling back to `LD_LIBRARY_PATH`.

### 11.3 macOS

* The dependency directory is `<bundle>/Contents/Frameworks/`.
* Bundled dependencies MUST have an install name of the form `@rpath/libSomething.dylib` (or
  `@rpath/SomeDependency.framework/Versions/A/SomeDependency`).
* The plug-in binary MUST carry `LC_RPATH` entries of `@loader_path/../Frameworks` and, for
  a dependency loading its own siblings, `@loader_path`.
* `@executable_path` MUST NOT be used. The executable is the host DAW, whose location is
  unknown and irrelevant; `@loader_path` is relative to the binary doing the loading, which
  is the plug-in.
* The library MUST be opened with `RTLD_NOW | RTLD_LOCAL`, for the reasons in §11.2.
* `install_name_tool` / `-install_name` fix-ups MUST be applied **before** `codesign`, since
  editing a Mach-O invalidates its signature.

### 11.4 Packaging guidance

* Bundle only what the plug-in genuinely needs. Every bundled library is a library the
  publisher now maintains and patches.
* MUST NOT bundle the C runtime, the system allocator, libc, `libstdc++`, or OS frameworks.
* SHOULD NOT bundle a library the host is likely to have already loaded at a different
  version unless it is loaded with local scope (§11.1–§11.3 ensure this).
* A pure-Rust plug-in with no C dependencies SHOULD ship no `Library/` or `Frameworks/`
  directory at all. That is the expected case for this SDK.

---

## 12. Versioning

Five version numbers exist and none of them substitutes for another.

| Version                 | Where it lives                                       | v1 value    | Governs                        |
| ----------------------- | ---------------------------------------------------- | ----------- | ------------------------------ |
| SDK version             | `sdk.version` / `DauxPluginEntryV1::sdk_version`      | e.g. `0.1.0` | Diagnostics only               |
| **Bundle format**       | `formatVersion` / `DAUxFormatVersion`                 | `1`         | Directory layout, this document |
| **ABI**                 | `abiVersion` / `DAUxAbiVersion`, and the entry struct | `1`         | Binary contract, abi-v1        |
| **Plug-in version**     | `plugin.version` / `CFBundleShortVersionString`       | e.g. `1.4.2` | Human-facing release identity  |
| **State schema**        | `DauxPluginDescriptorV1::state_schema_version`        | e.g. `3`    | Saved-project compatibility    |

Compatibility rules:

1. **Bundle format.** A host implementing format version *N* MUST load bundles whose
   `formatVersion` is ≤ *N* and MUST refuse those > *N*, with a diagnostic that names the
   required version. Unknown keys within a supported version MUST be ignored (§7.1).
2. **ABI.** `abiVersion` in the metadata is the ABI **major** version and exists so a scanner
   can skip a bundle before `dlopen`. It is advisory. The binary's
   `DauxPluginEntryV1::abi_version_major` is authoritative and abi-v1 §3's rejection rules
   apply without exception. Minor versions are tail extensions: a host MUST accept a lower
   or higher minor.
3. **Format and ABI are independent.** A format-version-2 bundle MAY carry an ABI-1 binary,
   and a future ABI 2 binary MAY ship in a format-version-1 bundle. Neither implies the
   other; there is no combined "AXT version".
4. **Plug-in version.** Purely informational. A host MUST NOT use it for compatibility
   decisions, MUST NOT refuse to load an older one, and MUST NOT treat a version change as an
   identity change. Identity is `plugin.id`, forever (abi-v1 §14).
5. **State schema.** Owned entirely by abi-v1 §12. A plug-in MUST load every schema version
   it has ever shipped or fail cleanly with `DAUX_ERR_VERSION`. The bundle format has no
   opinion; `stateSchemaVersion` in the manifest is a scanner hint and MUST match the
   descriptor.
6. **Downgrade.** Replacing a bundle with one carrying an older `plugin.version` MUST work.
   Users roll back. The saved state of the newer version may then fail to load — that is the
   plug-in's problem (rule 5), not the bundle format's.
7. **Ordering.** `plugin.version` is compared component-wise as unsigned decimal integers,
   `major.minor.patch[.build]`, missing components treated as 0. No pre-release or metadata
   syntax is defined in v1; a suffix MUST be ignored for ordering and preserved for display.

---

## 13. Signing (optional)

DAUx signing is **not required** in format version 1. A conforming host MUST load an
unsigned bundle without a warning that suggests the bundle is defective. This section
defines the shape of the optional model so that adding it later is not a format break.

### 13.1 Layout

```text
<bundle>/Signature/            (POSIX layout)
<bundle>/Contents/Signature/   (Apple layout)
├─ hashes.json
├─ signature.json
└─ certs/                      (optional, publisher-supplied chain)
```

If `Signature/` is absent the bundle is simply unsigned. If it is present it MUST be
well-formed; a malformed signature directory MUST be treated as "verification failed", never
as "unsigned".

### 13.2 Hash manifest and signature

```json
{
  "hashVersion": 1,
  "algorithm": "sha256",
  "files": [
    { "path": "Content/windows-x86_64/EQUZX.dll", "size": 2416640, "hash": "9f2c…" },
    { "path": "Resources/Shaders/spectrum.wgsl",  "size": 4211,    "hash": "1ab7…" },
    { "path": "manifest.json",                    "size": 812,     "hash": "c4e0…" }
  ]
}
```

* `path` is bundle-root relative, `/` separated, and subject to §10.2 in full. A hash
  manifest containing an illegal path MUST fail verification.
* `hash` is lowercase hex. `sha256` MUST be implemented; other algorithms MAY be added by a
  later `hashVersion`.
* The file list MUST cover **every** regular file in the bundle except those under
  `Signature/` (and `Contents/_CodeSignature/` on Apple). A file present on disk but absent
  from the list MUST fail verification — otherwise an attacker adds a payload instead of
  modifying one.
* Entries MUST be sorted byte-wise by `path`, and the document MUST be serialised
  deterministically, so that two builds of identical content produce identical bytes.
* Symlinks MUST NOT appear in a signed bundle.

```json
{
  "signatureVersion": 1,
  "publisherId": "studio.futureboard",
  "publisherName": "Futureboard Studio",
  "keyId": "2f1c9b…",
  "algorithm": "ed25519",
  "signedAt": "2026-01-31T12:00:00Z",
  "hashesSha256": "b31d…",
  "signature": "base64…"
}
```

The signature is computed over the exact bytes of `hashes.json`. `hashesSha256` lets a
verifier detect a truncated or swapped hash manifest before parsing it.

### 13.3 Coexistence with platform signing

* **macOS.** Apple signing is the real trust mechanism: `codesign`, hardened runtime,
  notarization, Gatekeeper. DAUx signing MUST NOT replace or interfere with it. Because any
  file added after signing invalidates the seal, the order is fixed:

  ```text
  build → install_name fix-ups → write Contents/Signature/ → codesign → notarize → staple
  ```

  `hashes.json` MUST exclude `Contents/_CodeSignature/`, which does not yet exist when it is
  written.
* **Windows.** Authenticode signs the PE files themselves and the signature lives inside the
  PE, so the order is: sign each `Content/{target}/*.dll` with Authenticode **first**, then
  compute `hashes.json` over the signed bytes. A host SHOULD surface the Authenticode
  publisher when present.
* **Linux.** No platform mechanism exists; distribution-level signing (package signatures)
  is orthogonal and MUST NOT be assumed.

### 13.4 Verification

* Verification is file I/O plus cryptography. It MUST happen on a **[main-thread]** or
  worker thread, at scan or load time, and MUST NOT happen on the audio thread — not in
  `process`, not in `activate`, not in any function reachable from them. There is no
  incremental "verify a bit per block" mode and none will be added.
* A host MUST NOT present an unverified or self-asserted `publisherName` as trusted. Absent
  a trust anchor, the correct UI is "signed by an unrecognised publisher", never "verified".
* Verification failure MUST be a load refusal, reported as such, and MUST NOT be silently
  downgraded to a warning.
* v1 defines no revocation, no PKI, no timestamping and no trust store. Those belong to the
  version that makes signing mandatory, if one ever does.

---

## 14. Conformance checklist

A directory conforms to AXT v1 when:

- [ ] it is a directory named `<BundleName>.axt` with `<BundleName>` per §2;
- [ ] exactly one of `manifest.json` or `Contents/Info.plist` is present (§4);
- [ ] the metadata parses, is within the §7.7 bounds, and declares `formatVersion` 1;
- [ ] `plugin.id` is reverse-DNS ASCII ≤127 bytes and matches a descriptor in the binary;
- [ ] `targets` is non-empty, has no duplicates, and every entry is from §3's closed set;
- [ ] every listed target has exactly one correctly named binary in its expected place (§3.2);
- [ ] every binary's machine type matches its target id (§15, `axt.binary.arch`);
- [ ] every binary exports `daux_plugin_entry_v1` and no other `daux_`-prefixed symbol;
- [ ] the entry struct passes abi-v1 §3 (magic, `abi_version_major == 1`, minimum `size`);
- [ ] no `macos-*` target appears in a POSIX bundle and no `windows-*`/`linux-*` target in an Apple one;
- [ ] every declared resource path is a legal logical path (§10.2) and every `required` one exists;
- [ ] every declared dependency is a single file name present in `Library/{target}/` or `Contents/Frameworks/`;
- [ ] no file inside the bundle is a symlink that resolves outside it;
- [ ] on Apple layout, `CFBundlePackageType` is `BNDL` and `CFBundleExecutable` names the file in `Contents/MacOS/`;
- [ ] the bundle contains no absolute path references in its metadata;
- [ ] parsing every metadata file with hostile input produces an error, never a panic.

---

## 15. What `daux validate` checks

`daux validate <bundle>` reports `ValidationIssue { severity, code, message }`. Codes are
stable strings; new codes MAY be added in a later format version, and a tool MUST NOT treat
an unknown code as fatal.

| Code                          | Severity | Check                                                              |
| ----------------------------- | -------- | ------------------------------------------------------------------ |
| `axt.bundle.not-a-directory`  | Error    | Path is a file, or does not exist                                   |
| `axt.bundle.extension`        | Error    | Name does not end in `.axt`                                         |
| `axt.bundle.name`             | Warning  | `<BundleName>` violates §2's character or length rules              |
| `axt.layout.unknown`          | Error    | Neither `manifest.json` nor `Contents/Info.plist` present           |
| `axt.layout.ambiguous`        | Error    | Both present (§4)                                                   |
| `axt.layout.target-mismatch`  | Error    | `macos-*` in a POSIX bundle, or the reverse                         |
| `axt.manifest.parse`          | Error    | Malformed JSON / plist                                              |
| `axt.manifest.too-large`      | Error    | Exceeds a §7.7 bound                                                |
| `axt.manifest.missing-field`  | Error    | A MUST field is absent                                              |
| `axt.manifest.format`         | Error    | `format` string wrong, or `formatVersion` unsupported               |
| `axt.manifest.unknown-key`    | Info     | Key not recognised in this format version                           |
| `axt.plugin.id`               | Error    | Not reverse-DNS ASCII, empty, or over 127 bytes                     |
| `axt.plugin.version`          | Warning  | Not `major.minor.patch[.build]`                                     |
| `axt.target.unknown`          | Error    | Target id outside §3's set                                          |
| `axt.target.duplicate`        | Error    | Repeated entry in `targets`                                         |
| `axt.target.universal-mix`    | Error    | `macos-universal` listed with a thin macOS target                   |
| `axt.binary.missing`          | Error    | No binary for a declared target                                     |
| `axt.binary.name`             | Error    | Binary stem ≠ `<BundleName>`                                        |
| `axt.binary.lib-prefix`       | Warning  | Linux binary is `lib<Name>.so` (tolerated, §3.2)                    |
| `axt.binary.extra-files`      | Info     | Extra files in `Content/{target}/`                                  |
| `axt.binary.orphan-target`    | Warning  | `Content/{target}/` exists but is not in `targets`                  |
| `axt.binary.arch`             | Error    | PE `Machine`, ELF `e_machine` or Mach-O `cputype` ≠ target id (§15.1) |
| `axt.binary.not-thin`         | Error    | Thin Mach-O where `macos-universal` was declared, or the reverse    |
| `axt.binary.entry-missing`    | Error    | `daux_plugin_entry_v1` not exported                                 |
| `axt.binary.extra-daux-symbol`| Warning  | Another exported `daux_`-prefixed symbol                            |
| `axt.abi.mismatch`            | Error    | Metadata `abiVersion` ≠ binary `abi_version_major`                  |
| `axt.abi.unsupported`         | Error    | Binary fails abi-v1 §3 (magic, version, `size`)                     |
| `axt.descriptor.id-mismatch`  | Error    | `plugin.id` matches no descriptor                                   |
| `axt.descriptor.duplicate-id` | Error    | Two descriptors share an id, here or across scanned bundles         |
| `axt.descriptor.metadata`     | Warning  | `name`/`vendor`/`version`/`category`/`capabilities` disagree with the descriptor |
| `axt.state.schema-mismatch`   | Warning  | `stateSchemaVersion` ≠ `DauxPluginDescriptorV1::state_schema_version` |
| `axt.resource.illegal-path`   | Error    | A declared resource path violates §10.2                             |
| `axt.resource.missing`        | Error    | A `required` resource does not exist                                |
| `axt.resource.escape`         | Error    | A file under `Resources/` is a symlink resolving outside the bundle |
| `axt.resource.absolute`       | Error    | Metadata contains an absolute path or drive letter                  |
| `axt.dependency.missing`      | Error    | Declared dependency absent from `Library/{target}` / `Frameworks`   |
| `axt.dependency.path`         | Error    | Dependency entry is not a bare file name                            |
| `axt.dependency.undeclared`   | Info     | File present in the dependency directory but not declared           |
| `axt.dependency.rpath`        | Warning  | Missing `DT_RUNPATH` `$ORIGIN` (Linux) or `@rpath`/`LC_RPATH` (macOS) |
| `axt.dependency.system-lib`   | Warning  | A bundled dependency looks like a system library (§11.4)            |
| `axt.graphics.declaration`    | Warning  | `graphics.enabled` disagrees with `DAUX_CAP_HAS_GUI`                |
| `axt.graphics.framework`      | Warning  | Unknown `framework`/`renderer` value                                |
| `axt.plist.package-type`      | Error    | `CFBundlePackageType` ≠ `BNDL`                                      |
| `axt.plist.executable`        | Error    | `CFBundleExecutable` does not name the file in `Contents/MacOS/`    |
| `axt.plist.identifier`        | Error    | `CFBundleIdentifier` ≠ plug-in id                                   |
| `axt.plist.binary-format`     | Warning  | `Info.plist` is a binary plist (§8)                                 |
| `axt.signature.malformed`     | Error    | `Signature/` present but unreadable or inconsistent (§13.1)         |
| `axt.signature.unlisted-file` | Error    | File present but absent from `hashes.json`                          |
| `axt.signature.hash-mismatch` | Error    | Content hash differs from `hashes.json`                             |
| `axt.export.compat`           | Warning  | A capability cannot be expressed in a requested export format       |

### 15.1 Architecture checks

`daux validate` inspects the binary header without loading it:

| Target id         | Header field                              | Expected value                        |
| ----------------- | ----------------------------------------- | ------------------------------------- |
| `windows-x86_64`  | PE `IMAGE_FILE_HEADER.Machine`            | `0x8664` (AMD64), PE32+               |
| `windows-aarch64` | PE `IMAGE_FILE_HEADER.Machine`            | `0xAA64` (ARM64), PE32+               |
| `linux-x86_64`    | ELF `e_machine`, `EI_CLASS`               | `62` (EM_X86_64), `ELFCLASS64`        |
| `linux-aarch64`   | ELF `e_machine`, `EI_CLASS`               | `183` (EM_AARCH64), `ELFCLASS64`      |
| `macos-x86_64`    | Mach-O `cputype`                          | `0x0100_0007` (CPU_TYPE_X86_64)       |
| `macos-arm64`     | Mach-O `cputype`                          | `0x0100_000C` (CPU_TYPE_ARM64)        |
| `macos-universal` | Fat header magic, then both `cputype`s    | `0xCAFEBABE` / `0xBEBAFECA`, containing both of the above |

Validation MUST bound every read against the file length and MUST fail with a diagnostic
rather than panic on a truncated or crafted header. Platform-specific inspection (Windows PE
subsystem, macOS `codesign --verify` consistency, Linux ELF `DT_RUNPATH`) is OPTIONAL and
MAY be skipped when the corresponding tooling is unavailable; skipping MUST be reported at
`Info` severity, never silently treated as a pass.

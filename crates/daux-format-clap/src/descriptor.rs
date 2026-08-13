//! Turning a [`PluginDescriptor`] into a `clap_plugin_descriptor` that outlives the call.
//!
//! CLAP requires the descriptor pointer a factory hands back to stay valid until
//! `clap_plugin_entry::deinit`, and every field in it is a bare `const char *`. That means
//! the adapter has to own the bytes. [`OwnedDescriptor`] is that ownership: the C strings
//! and the NULL-terminated feature array live in heap allocations the struct holds, and the
//! `#[repr(C)]` view points into them.
//!
//! `[main-thread]` — everything here allocates.

use std::ffi::CString;

use daux_plugin_api::{Capabilities, Category, PluginDescriptor};

use crate::abi::{ClapPluginDescriptor, ClapVersion};

/// A `clap_plugin_descriptor` and everything it points at.
///
/// # Why this is not self-referential
///
/// The raw pointers in `view` address the *heap* buffers of the `CString`s and of
/// `feature_ptrs`, never the `OwnedDescriptor` struct itself. Moving the struct — into a
/// `Vec`, into a `Box` — moves the pointers but not the bytes they point at, so the view
/// stays valid without pinning. What *would* invalidate it is mutating a field after
/// construction, which is why every field is private and there is no setter.
pub struct OwnedDescriptor {
    /// The C view handed to the host.
    view: ClapPluginDescriptor,
    /// Backing storage for the string fields, in the order the view names them.
    ///
    /// Never read again after construction — it exists so the bytes `view`'s pointers
    /// address stay alive — which is exactly what `dead_code` is for and why it is silenced
    /// here rather than removed.
    #[allow(dead_code)]
    strings: Vec<CString>,
    /// Backing storage for the feature strings. Kept alive for the same reason as
    /// `strings`.
    #[allow(dead_code)]
    features: Vec<CString>,
    /// The NULL-terminated array `view.features` points at. Kept alive for the same reason
    /// as `strings`.
    #[allow(dead_code)]
    feature_ptrs: Vec<*const core::ffi::c_char>,
    /// The DAUx descriptor this was built from, kept so instances can consult it without
    /// asking the factory again.
    daux: PluginDescriptor,
}

impl core::fmt::Debug for OwnedDescriptor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OwnedDescriptor")
            .field("id", &self.daux.id.as_str())
            .field("name", &self.daux.name)
            .finish_non_exhaustive()
    }
}

// SAFETY: the raw pointers inside are owning pointers into this value's own heap buffers,
// which no other thread can reach and which are never mutated after construction. Moving
// the value between threads moves the whole graph, so there is nothing to race on. This is
// the same reasoning that makes `CString` itself `Send`/`Sync`.
unsafe impl Send for OwnedDescriptor {}
// SAFETY: after construction the value is immutable, so shared access from several threads
// only ever reads bytes that never change.
unsafe impl Sync for OwnedDescriptor {}

impl OwnedDescriptor {
    /// `[main-thread]` Builds the CLAP view of a DAUx descriptor.
    ///
    /// Strings containing a NUL are truncated at it rather than refused, so one malformed
    /// field cannot make a whole plug-in invisible;
    /// [`compatibility_report`](crate::compatibility_report) reports the truncation.
    #[must_use]
    pub fn new(daux: PluginDescriptor) -> Self {
        let version = version_text(&daux);
        let strings: Vec<CString> = [
            daux.id.as_str(),
            daux.name.as_str(),
            daux.vendor.as_str(),
            daux.url.as_str(),
            // CLAP has a `manual_url` slot and DAUx has no separate field for it; the
            // product URL is the closest true answer, and an empty string is honest when
            // there is none.
            daux.url.as_str(),
            daux.support_url.as_str(),
            version.as_str(),
            daux.description.as_str(),
        ]
        .into_iter()
        .map(to_cstring)
        .collect();

        let features: Vec<CString> = clap_features(&daux).iter().map(|f| to_cstring(f)).collect();
        let mut feature_ptrs: Vec<*const core::ffi::c_char> =
            features.iter().map(|c| c.as_ptr()).collect();
        feature_ptrs.push(core::ptr::null());

        let view = ClapPluginDescriptor {
            clap_version: ClapVersion::CURRENT,
            id: strings[0].as_ptr(),
            name: strings[1].as_ptr(),
            vendor: strings[2].as_ptr(),
            url: strings[3].as_ptr(),
            manual_url: strings[4].as_ptr(),
            support_url: strings[5].as_ptr(),
            version: strings[6].as_ptr(),
            description: strings[7].as_ptr(),
            features: feature_ptrs.as_ptr(),
        };

        Self {
            view,
            strings,
            features,
            feature_ptrs,
            daux,
        }
    }

    /// `[main-thread]` The C view, valid for as long as this value lives.
    #[must_use]
    pub fn view(&self) -> *const ClapPluginDescriptor {
        core::ptr::from_ref(&self.view)
    }

    /// `[main-thread]` The DAUx descriptor this was built from.
    #[must_use]
    pub const fn daux(&self) -> &PluginDescriptor {
        &self.daux
    }

    /// `[main-thread]` The permanent plug-in id, for matching `create_plugin`'s argument.
    #[must_use]
    pub fn id(&self) -> &str {
        self.daux.id.as_str()
    }
}

/// The product version rendered the way CLAP wants it: free-form text. `[main-thread]`
fn version_text(d: &PluginDescriptor) -> String {
    let (major, minor, patch, build) = d.version.to_parts();
    if build == 0 {
        format!("{major}.{minor}.{patch}")
    } else {
        format!("{major}.{minor}.{patch}.{build}")
    }
}

/// Converts to a C string, truncating at the first interior NUL. `[main-thread]`
fn to_cstring(s: &str) -> CString {
    match s.find('\0') {
        Some(cut) => CString::new(&s[..cut]).unwrap_or_default(),
        None => CString::new(s).unwrap_or_default(),
    }
}

/// `[main-thread]` The CLAP feature list for a descriptor.
///
/// CLAP's feature array is how a host files a plug-in: the first entries must be the
/// standard *primary* features (`instrument`, `audio-effect`, `note-effect`, `analyzer`),
/// and everything after them is free-form. DAUx says the same thing twice — once in
/// [`Category`] and once in [`Capabilities`] — so both are consulted and the union is
/// emitted, de-duplicated and in a stable order.
///
/// A plug-in that declares nothing still gets `audio-effect`, because a CLAP descriptor
/// with an empty feature list is filed under nothing at all in most hosts.
#[must_use]
pub fn clap_features(d: &PluginDescriptor) -> Vec<String> {
    /// Appends `feature` unless it is already there, so the same tag reached from the
    /// category and from the capability set only appears once.
    fn push(out: &mut Vec<String>, feature: &str) {
        if !out.iter().any(|f| f == feature) {
            out.push(feature.to_owned());
        }
    }

    let caps = d.capabilities;
    let mut out: Vec<String> = Vec::new();

    if caps.is_instrument() || d.category == Category::Instrument {
        push(&mut out, "instrument");
    }
    if caps.is_midi_effect() || d.category == Category::MidiEffect {
        push(&mut out, "note-effect");
    }
    if caps.is_analyzer() || d.category == Category::Analyzer {
        push(&mut out, "analyzer");
    }
    if caps.is_audio_effect()
        || matches!(
            d.category,
            Category::Effect | Category::Generator | Category::Utility
        )
    {
        push(&mut out, "audio-effect");
    }
    if out.is_empty() {
        push(&mut out, "audio-effect");
    }

    for tag in &d.features {
        let tag = tag.split('\0').next().unwrap_or_default().trim();
        if !tag.is_empty() {
            push(&mut out, tag);
        }
    }
    out
}

/// `[main-thread]` Capability bits that have a direct CLAP equivalent, for documentation
/// and for tests that assert the mapping did not silently change.
#[must_use]
pub const fn clap_expressible_capabilities() -> Capabilities {
    Capabilities::AUDIO_EFFECT
        .with_instrument()
        .with_midi_effect()
        .with_analyzer()
        .with_midi_input()
        .with_midi_output()
        .with_midi2()
        .with_sidechain()
        .with_sample_accurate_auto()
        .with_note_expression()
        .with_has_gui()
        .with_offline_render()
        .with_hard_realtime()
        .with_latency_dynamic()
        .with_tail_infinite()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::CStr;
    use daux_plugin_api::Version;

    fn descriptor() -> PluginDescriptor {
        PluginDescriptor::builder("com.example.verb", "Verb")
            .vendor("Example Audio")
            .version(Version::new(2, 1, 3))
            .description("A plate reverb.")
            .url("https://example.com/verb")
            .support_url("https://example.com/support")
            .capabilities(Capabilities::AUDIO_EFFECT)
            .features(["reverb", "stereo"])
            .build()
            .unwrap()
    }

    /// Reads a field back exactly the way a host would.
    ///
    /// # Safety
    ///
    /// `p` must be a live NUL-terminated string.
    unsafe fn read(p: *const core::ffi::c_char) -> String {
        // SAFETY: the caller guarantees `p` points at a live NUL-terminated string; every
        // call site passes a field of a descriptor that is still alive.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }

    #[test]
    fn every_string_field_reaches_the_host_intact() {
        let owned = OwnedDescriptor::new(descriptor());
        let view = owned.view();
        // SAFETY: `owned` is alive for the whole test, so its view and every string it
        // points at are valid.
        let v = unsafe { &*view };
        // SAFETY: as above — each field is a live NUL-terminated string owned by `owned`.
        unsafe {
            assert_eq!(read(v.id), "com.example.verb");
            assert_eq!(read(v.name), "Verb");
            assert_eq!(read(v.vendor), "Example Audio");
            assert_eq!(read(v.url), "https://example.com/verb");
            assert_eq!(read(v.manual_url), "https://example.com/verb");
            assert_eq!(read(v.support_url), "https://example.com/support");
            assert_eq!(read(v.version), "2.1.3");
            assert_eq!(read(v.description), "A plate reverb.");
        }
        assert_eq!(v.clap_version, ClapVersion::CURRENT);
    }

    #[test]
    fn the_feature_array_is_null_terminated_and_ordered() {
        let owned = OwnedDescriptor::new(descriptor());
        // SAFETY: `owned` is alive, so its view is valid.
        let v = unsafe { &*owned.view() };
        let mut features = Vec::new();
        let mut i = 0isize;
        loop {
            // SAFETY: the array `v.features` points at is NULL-terminated by construction,
            // so walking it until the NULL never reads past the end.
            let p = unsafe { *v.features.offset(i) };
            if p.is_null() {
                break;
            }
            // SAFETY: every non-null entry is a live NUL-terminated string owned by `owned`.
            features.push(unsafe { read(p) });
            i += 1;
            assert!(i < 64, "the feature array must be NULL-terminated");
        }
        assert_eq!(features, ["audio-effect", "reverb", "stereo"]);
    }

    #[test]
    fn moving_the_descriptor_does_not_dangle_its_pointers() {
        let owned = OwnedDescriptor::new(descriptor());
        let boxed = Box::new(owned);
        let moved = [*boxed];
        // SAFETY: `moved` owns the descriptor; moving the struct moved the pointers but not
        // the heap bytes they address.
        let v = unsafe { &*moved[0].view() };
        // SAFETY: `v.id` is still the live id string of the moved-into descriptor.
        assert_eq!(unsafe { read(v.id) }, "com.example.verb");
    }

    #[test]
    fn a_build_number_appears_in_the_version_text() {
        let d = PluginDescriptor::builder("com.example.x", "X")
            .version(Version::new(1, 2, 3).with_build(77))
            .build()
            .unwrap();
        let owned = OwnedDescriptor::new(d);
        // SAFETY: `owned` is alive for the whole test.
        let v = unsafe { &*owned.view() };
        // SAFETY: `v.version` is a live NUL-terminated string owned by `owned`.
        assert_eq!(unsafe { read(v.version) }, "1.2.3.77");
    }

    #[test]
    fn an_interior_nul_truncates_instead_of_producing_an_empty_field() {
        let mut d = descriptor();
        d.name = "Ver\0b".to_owned();
        let owned = OwnedDescriptor::new(d);
        // SAFETY: `owned` is alive for the whole test.
        let v = unsafe { &*owned.view() };
        // SAFETY: `v.name` is a live NUL-terminated string owned by `owned`.
        assert_eq!(unsafe { read(v.name) }, "Ver");
    }

    #[test]
    fn features_come_from_capabilities_and_category_without_duplicates() {
        let instrument = PluginDescriptor::builder("com.example.synth", "Synth")
            .category(Category::Instrument)
            .capabilities(Capabilities::INSTRUMENT.with_midi_input())
            .feature("instrument")
            .feature("synthesizer")
            .build()
            .unwrap();
        assert_eq!(clap_features(&instrument), ["instrument", "synthesizer"]);

        let arp = PluginDescriptor::builder("com.example.arp", "Arp")
            .category(Category::MidiEffect)
            .capabilities(Capabilities::MIDI_EFFECT)
            .build()
            .unwrap();
        assert_eq!(clap_features(&arp), ["note-effect"]);

        let both = PluginDescriptor::builder("com.example.both", "Both")
            .category(Category::Analyzer)
            .capabilities(Capabilities::AUDIO_EFFECT.with_analyzer())
            .build()
            .unwrap();
        assert_eq!(both.category, Category::Analyzer);
        assert_eq!(clap_features(&both), ["analyzer", "audio-effect"]);
    }

    #[test]
    fn a_plugin_that_declares_nothing_still_gets_a_primary_feature() {
        let d = PluginDescriptor::builder("com.example.bare", "Bare")
            .category(Category::Unknown)
            .capabilities(Capabilities::NONE)
            .build()
            .unwrap();
        assert_eq!(clap_features(&d), ["audio-effect"]);
    }

    #[test]
    fn blank_and_nul_bearing_tags_are_dropped_or_trimmed() {
        let mut d = descriptor();
        d.features = vec![
            "  spaced  ".to_owned(),
            "\0".to_owned(),
            "keep\0drop".to_owned(),
        ];
        assert_eq!(
            clap_features(&d),
            ["audio-effect", "spaced", "keep"],
            "an all-NUL tag must vanish and a partial one must keep its prefix"
        );
    }

    #[test]
    fn the_expressible_capability_set_is_the_documented_one() {
        let expressible = clap_expressible_capabilities();
        // Everything CLAP genuinely cannot say must be absent, or `compatibility_report`
        // and this table have drifted apart.
        assert!(!expressible.is_shared_texture_gui());
        assert!(!expressible.is_requires_gui());
        assert!(!expressible.is_sandbox_safe());
        assert!(!expressible.is_stereo_only());
        assert!(!expressible.is_dynamic_buses());
        assert!(expressible.is_tail_infinite());
        assert!(expressible.is_midi2());
    }
}

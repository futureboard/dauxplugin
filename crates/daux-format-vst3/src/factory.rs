//! `IPluginFactory`, `IPluginFactory2` and `IPluginFactory3`: what the module exports.
//!
//! A host loads the library, calls `GetPluginFactory`, enumerates the classes and creates the
//! ones it wants. That is the whole scanning path, so everything here must be cheap and must
//! not touch DSP: a user with three hundred plug-ins installed pays this cost on every
//! start-up.
//!
//! # No singleton
//!
//! `GetPluginFactory` builds a **new** factory object each time it is called and hands back
//! one reference. Steinberg's own implementation returns a process-wide singleton with an
//! extra `addRef`, which is a global mutable object in a library that must support hundreds
//! of coexisting instances. A fresh object per call costs one small allocation on a path that
//! runs once per scan, and removes the singleton entirely.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use daux_plugin_api::{DauxFactory, PluginDescriptor, PluginInstance};

use crate::api::{
    self, IPluginFactoryVtbl, K_MANY_INSTANCES, K_VST_AUDIO_EFFECT_CLASS, K_VST_SDK_VERSION,
    PClassInfo, PClassInfo2, PClassInfoW, PFactoryInfo, factory_flags,
};
use crate::cid::class_id;
use crate::com::{FidString, TResult, TUid, iid_eq, result};
use crate::component::Vst3Component;
use crate::guard::Poison;
use crate::mapping;
use crate::strings;

/// One exported plug-in class, resolved once when the factory is built.
///
/// Shared with every instance of that class through an `Arc`, so a descriptor's strings are
/// built once per module rather than once per instance.
#[derive(Debug)]
pub struct ClassEntry {
    /// The DAUx description this class was derived from.
    pub descriptor: PluginDescriptor,
    /// The permanent VST3 class id, derived from [`PluginDescriptor::id`].
    pub cid: TUid,
    /// The `|`-separated VST3 subcategory string.
    pub subcategories: String,
    /// The product version as VST3 shows it.
    pub version: String,
}

impl ClassEntry {
    /// `[main-thread]` Resolves one descriptor into everything VST3 asks about it.
    #[must_use]
    pub fn new(descriptor: PluginDescriptor) -> Self {
        let cid = class_id(descriptor.id.as_str());
        let subcategories = mapping::subcategories(&descriptor);
        let version = descriptor.version.to_string();
        Self {
            descriptor,
            cid,
            subcategories,
            version,
        }
    }
}

/// The module's factory object.
#[repr(C)]
pub struct Vst3Factory {
    vtbl: *const IPluginFactoryVtbl,
    ref_count: AtomicU32,
    poison: Poison,
    factory: Box<dyn DauxFactory>,
    classes: Vec<Arc<ClassEntry>>,
}

static FACTORY_VTBL: IPluginFactoryVtbl = IPluginFactoryVtbl {
    query_interface: Vst3Factory::query_interface,
    add_ref: Vst3Factory::add_ref,
    release: Vst3Factory::release,
    get_factory_info: Vst3Factory::get_factory_info,
    count_classes: Vst3Factory::count_classes,
    get_class_info: Vst3Factory::get_class_info,
    create_instance: Vst3Factory::create_instance,
    get_class_info2: Vst3Factory::get_class_info2,
    get_class_info_unicode: Vst3Factory::get_class_info_unicode,
    set_host_context: Vst3Factory::set_host_context,
};

impl Vst3Factory {
    /// `[main-thread]` Wraps a DAUx factory, returning its `IPluginFactory` with one
    /// reference the caller owns.
    ///
    /// Two plug-ins whose ids hash to the same class id would be indistinguishable to a
    /// host, so the second is dropped rather than shadowing the first. With a 122-bit hash
    /// this cannot happen by accident; it can happen if a factory reports the same plug-in
    /// twice.
    #[must_use]
    pub fn create(factory: Box<dyn DauxFactory>) -> *mut c_void {
        let mut classes: Vec<Arc<ClassEntry>> = Vec::new();
        for descriptor in factory.descriptors() {
            let entry = ClassEntry::new(descriptor);
            if classes.iter().any(|c| c.cid == entry.cid) {
                continue;
            }
            classes.push(Arc::new(entry));
        }
        let object = Box::new(Self {
            vtbl: &raw const FACTORY_VTBL,
            ref_count: AtomicU32::new(1),
            poison: Poison::new(),
            factory,
            classes,
        });
        Box::into_raw(object).cast::<c_void>()
    }

    /// # Safety
    ///
    /// `this` must be a live pointer returned by [`Vst3Factory::create`].
    unsafe fn from_this<'a>(this: *mut c_void) -> &'a Self {
        // SAFETY: the vtable is the first field, so the head's address is the object's. The
        // borrow is shared because everything mutable is an atomic.
        unsafe { &*this.cast::<Self>() }
    }

    // ---- FUnknown --------------------------------------------------------------------

    unsafe extern "system" fn query_interface(
        this: *mut c_void,
        iid: *const TUid,
        obj: *mut *mut c_void,
    ) -> TResult {
        if this.is_null() || obj.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: `obj` was checked non-null and is caller-owned.
        unsafe { *obj = core::ptr::null_mut() };
        // SAFETY: `iid` is the host's; `iid_eq` tolerates null. All three factory
        // interfaces are one inheritance chain, so one vtable and one pointer serve them.
        let wanted = unsafe {
            iid_eq(iid, &api::IFUNKNOWN_IID)
                || iid_eq(iid, &api::IPLUGIN_FACTORY_IID)
                || iid_eq(iid, &api::IPLUGIN_FACTORY2_IID)
                || iid_eq(iid, &api::IPLUGIN_FACTORY3_IID)
        };
        if !wanted {
            return result::NO_INTERFACE;
        }
        // SAFETY: a live factory.
        let me = unsafe { Self::from_this(this) };
        me.ref_count.fetch_add(1, Ordering::AcqRel);
        // SAFETY: `obj` was checked non-null.
        unsafe { *obj = this };
        result::OK
    }

    unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        if this.is_null() {
            return 0;
        }
        // SAFETY: a live factory.
        let me = unsafe { Self::from_this(this) };
        me.ref_count.fetch_add(1, Ordering::AcqRel) + 1
    }

    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        if this.is_null() {
            return 0;
        }
        // SAFETY: a live factory the caller owns a reference to.
        let me = unsafe { Self::from_this(this) };
        let remaining = me.ref_count.fetch_sub(1, Ordering::AcqRel) - 1;
        if remaining == 0 {
            // SAFETY: the count reached zero, so this is the last reference; the pointer came
            // from `Box::into_raw` in `create`.
            drop(unsafe { Box::from_raw(this.cast::<Self>()) });
        }
        remaining
    }

    // ---- IPluginFactory --------------------------------------------------------------

    unsafe extern "system" fn get_factory_info(
        this: *mut c_void,
        info: *mut PFactoryInfo,
    ) -> TResult {
        if this.is_null() || info.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live factory.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            let first = me.classes.first();
            let mut out = PFactoryInfo {
                vendor: [0; 64],
                url: [0; 256],
                email: [0; 128],
                // `kUnicode` says the `char8` fields are UTF-8, which is what this adapter
                // writes; without it a host is entitled to read them as Latin-1.
                flags: factory_flags::UNICODE | factory_flags::CLASSES_DISCARDABLE,
            };
            if let Some(class) = first {
                strings::write_utf8(&mut out.vendor, &class.descriptor.vendor);
                strings::write_utf8(&mut out.url, &class.descriptor.url);
                strings::write_utf8(&mut out.email, &class.descriptor.support_url);
            }
            // SAFETY: `info` was checked non-null and is caller-owned.
            unsafe { *info = out };
            result::OK
        })
    }

    unsafe extern "system" fn count_classes(this: *mut c_void) -> i32 {
        if this.is_null() {
            return 0;
        }
        // SAFETY: a live factory.
        let me = unsafe { Self::from_this(this) };
        me.poison
            .call_value(0, || i32::try_from(me.classes.len()).unwrap_or(i32::MAX))
    }

    unsafe extern "system" fn get_class_info(
        this: *mut c_void,
        index: i32,
        info: *mut PClassInfo,
    ) -> TResult {
        if this.is_null() || info.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live factory.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            let Some(class) = me.class_at(index) else {
                return result::INVALID_ARGUMENT;
            };
            let mut out = PClassInfo {
                cid: class.cid,
                cardinality: K_MANY_INSTANCES,
                category: [0; 32],
                name: [0; 64],
            };
            strings::write_utf8(&mut out.category, K_VST_AUDIO_EFFECT_CLASS);
            strings::write_utf8(&mut out.name, &class.descriptor.name);
            // SAFETY: `info` was checked non-null and is caller-owned.
            unsafe { *info = out };
            result::OK
        })
    }

    unsafe extern "system" fn get_class_info2(
        this: *mut c_void,
        index: i32,
        info: *mut PClassInfo2,
    ) -> TResult {
        if this.is_null() || info.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live factory.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            let Some(class) = me.class_at(index) else {
                return result::INVALID_ARGUMENT;
            };
            let mut out = PClassInfo2 {
                cid: class.cid,
                cardinality: K_MANY_INSTANCES,
                category: [0; 32],
                name: [0; 64],
                // Never `kDistributable`: the component and the controller are one object.
                class_flags: 0,
                subcategories: [0; 128],
                vendor: [0; 64],
                version: [0; 64],
                sdk_version: [0; 64],
            };
            strings::write_utf8(&mut out.category, K_VST_AUDIO_EFFECT_CLASS);
            strings::write_utf8(&mut out.name, &class.descriptor.name);
            strings::write_utf8(&mut out.subcategories, &class.subcategories);
            strings::write_utf8(&mut out.vendor, &class.descriptor.vendor);
            strings::write_utf8(&mut out.version, &class.version);
            strings::write_utf8(&mut out.sdk_version, K_VST_SDK_VERSION);
            // SAFETY: `info` was checked non-null and is caller-owned.
            unsafe { *info = out };
            result::OK
        })
    }

    unsafe extern "system" fn get_class_info_unicode(
        this: *mut c_void,
        index: i32,
        info: *mut PClassInfoW,
    ) -> TResult {
        if this.is_null() || info.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live factory.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            let Some(class) = me.class_at(index) else {
                return result::INVALID_ARGUMENT;
            };
            let mut out = PClassInfoW {
                cid: class.cid,
                cardinality: K_MANY_INSTANCES,
                category: [0; 32],
                name: [0; 64],
                class_flags: 0,
                subcategories: [0; 128],
                vendor: [0; 64],
                version: [0; 64],
                sdk_version: [0; 64],
            };
            strings::write_utf8(&mut out.category, K_VST_AUDIO_EFFECT_CLASS);
            strings::write_utf16(&mut out.name, &class.descriptor.name);
            strings::write_utf8(&mut out.subcategories, &class.subcategories);
            strings::write_utf16(&mut out.vendor, &class.descriptor.vendor);
            strings::write_utf16(&mut out.version, &class.version);
            strings::write_utf16(&mut out.sdk_version, K_VST_SDK_VERSION);
            // SAFETY: `info` was checked non-null and is caller-owned.
            unsafe { *info = out };
            result::OK
        })
    }

    unsafe extern "system" fn set_host_context(
        this: *mut c_void,
        _context: *mut c_void,
    ) -> TResult {
        if this.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // The host application object is not retained: this adapter asks the host for
        // nothing a plug-in cannot get through `HostServices`, and holding a reference to it
        // for the module's lifetime is how a plug-in keeps a dead host alive.
        result::OK
    }

    unsafe extern "system" fn create_instance(
        this: *mut c_void,
        cid: FidString,
        iid: FidString,
        obj: *mut *mut c_void,
    ) -> TResult {
        if this.is_null() || obj.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: `obj` was checked non-null and is caller-owned.
        unsafe { *obj = core::ptr::null_mut() };
        if cid.is_null() || iid.is_null() {
            return result::INVALID_ARGUMENT;
        }
        // SAFETY: a live factory.
        let me = unsafe { Self::from_this(this) };
        me.poison.call(|| {
            // VST3 passes both ids as `FIDString`, which is a `const char*` pointing at
            // sixteen raw bytes rather than a string.
            let cid = cid.cast::<TUid>();
            let iid = iid.cast::<TUid>();

            // SAFETY: a conforming host points `cid` at sixteen readable bytes.
            let Some(class) = me
                .classes
                .iter()
                .find(|c| unsafe { iid_eq(cid, &c.cid) })
                .cloned()
            else {
                return result::NO_INTERFACE;
            };

            let Ok(plugin) = me.factory.create(class.descriptor.id.as_str()) else {
                return result::INTERNAL_ERROR;
            };
            let instance = PluginInstance::with_descriptor(plugin, class.descriptor.clone());
            let component = Vst3Component::create(instance, class);

            // The object is born with one reference. `queryInterface` adds the one the host
            // will own, and the original is released whether it succeeded or not — so a
            // refused interface frees the object instead of leaking it.
            // SAFETY: `component` was just created and `iid`/`obj` are the host's.
            let status = unsafe {
                let head = Vst3Component::as_com(component);
                let vtbl = *head.cast::<*const crate::api::IComponentVtbl>();
                ((*vtbl).query_interface)(head, iid, obj)
            };
            // SAFETY: gives back the reference `create` started with.
            unsafe {
                let head = Vst3Component::as_com(component);
                let vtbl = *head.cast::<*const crate::api::IComponentVtbl>();
                ((*vtbl).release)(head);
            }
            status
        })
    }

    /// The class at a VST3 index, or `None` when the host asks for one that is not there.
    fn class_at(&self, index: i32) -> Option<&Arc<ClassEntry>> {
        self.classes.get(usize::try_from(index).ok()?)
    }
}

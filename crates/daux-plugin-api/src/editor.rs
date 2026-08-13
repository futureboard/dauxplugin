//! Re-typing [`DauxPlugin::create_editor`]'s opaque return value into a real editor.
//!
//! `daux-core` must not depend on `daux-graphics` — the object model has no business knowing
//! what a window is — so [`DauxPlugin::create_editor`] returns `Option<Box<dyn Any>>`. This
//! module is the one place that opacity is undone.
//!
//! # The contract, and why it is shaped this way
//!
//! `Box<dyn Any>` can only be downcast to a **sized** type, so a plug-in cannot simply return
//! `Box::new(MyEditor)` and have the framework recover a `Box<dyn DauxGraphic>`: the
//! framework does not know `MyEditor`. What the `Any` must contain is therefore the already
//! type-erased `Box<dyn DauxGraphic>`, which is itself sized. [`editor`] does exactly that
//! wrapping, and it is the only thing a plug-in author should ever call:
//!
//! ```
//! use daux_plugin_api::prelude::*;
//! # use daux_plugin_api::daux_graphics::{GraphicCapabilities, GraphicDescriptor,
//! #     GraphicFramework, GraphicProfile, GraphicRenderer, PresentationMode};
//! # struct MyEditor;
//! # impl DauxGraphic for MyEditor {
//! #     fn descriptor(&self) -> GraphicDescriptor {
//! #         GraphicDescriptor::fixed(
//! #             GraphicCapabilities::new().with(GraphicProfile::new(
//! #                 GraphicFramework::Custom, GraphicRenderer::Software,
//! #                 PresentationMode::EmbeddedSurface)),
//! #             LogicalSize::new(200.0, 100.0))
//! #     }
//! #     fn open(&mut self, _ctx: &mut GraphicContext<'_>) -> DauxGraphicResult<()> { Ok(()) }
//! #     fn resize(&mut self, _size: PhysicalSize) -> DauxGraphicResult<()> { Ok(()) }
//! #     fn close(&mut self) {}
//! # }
//! # struct MyPlugin;
//! impl MyPlugin {
//!     fn create_editor(&mut self) -> Option<Box<dyn std::any::Any>> {
//!         editor(MyEditor)
//!     }
//! }
//! ```
//!
//! A plug-in that wraps it wrongly produces a clear [`ErrorKind::Plugin`] error naming the
//! fix, never a panic and never a silently editor-less plug-in.

use std::any::Any;

use daux_core::{DauxError, DauxPlugin, DauxResult, ErrorKind};
use daux_graphics::DauxGraphic;

/// The one message a mis-wrapped editor produces, kept in one place so the test and the
/// error cannot drift apart.
const MISWRAPPED: &str = "create_editor returned a value that is not a Box<dyn DauxGraphic>; \
                          build the return value with daux_plugin_api::editor(..) rather than \
                          boxing the editor directly";

/// [main-thread] Wraps an editor for return from [`DauxPlugin::create_editor`].
///
/// The `Option` is deliberate: `create_editor` returns one, so a plug-in body is just
/// `editor(MyEditor::new())`.
#[must_use]
pub fn editor<G: DauxGraphic + 'static>(graphic: G) -> Option<Box<dyn Any>> {
    Some(boxed_editor(Box::new(graphic)))
}

/// [main-thread] Wraps an already type-erased editor, for a plug-in that chooses its backend
/// at run time and therefore holds a `Box<dyn DauxGraphic>` rather than a concrete type.
#[must_use]
pub fn boxed_editor(graphic: Box<dyn DauxGraphic>) -> Box<dyn Any> {
    Box::new(graphic)
}

/// [main-thread] Recovers the editor a plug-in wrapped with [`editor`].
///
/// # Errors
///
/// [`ErrorKind::Plugin`] when the value was not produced by [`editor`] or
/// [`boxed_editor`]. The offending value is dropped rather than leaked, and nothing panics:
/// a plug-in bug must cost the user their editor, not their session.
pub fn downcast_editor(value: Box<dyn Any>) -> DauxResult<Box<dyn DauxGraphic>> {
    match value.downcast::<Box<dyn DauxGraphic>>() {
        Ok(boxed) => Ok(*boxed),
        Err(_) => Err(DauxError::from_static(ErrorKind::Plugin, MISWRAPPED)),
    }
}

/// [main-thread] Creates `plugin`'s editor and re-types it.
///
/// `Ok(None)` means the plug-in is headless, which is a normal answer and not an error.
///
/// The editor's lifetime is independent of the processor's: this may be called repeatedly,
/// while audio is running, and dropping the result must never touch DSP state.
///
/// # Errors
///
/// [`ErrorKind::Plugin`] when the plug-in returned something that is not a
/// `Box<dyn DauxGraphic>`.
pub fn take_editor(plugin: &mut dyn DauxPlugin) -> DauxResult<Option<Box<dyn DauxGraphic>>> {
    plugin.create_editor().map(downcast_editor).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{Counts, Spy, SpyEditor};
    use daux_audio::BusLayout;
    use daux_core::{DauxController, DauxProcessor, PluginDescriptor};
    use daux_graphics::{GraphicContext, PhysicalSize, ScaleFactor, WindowTarget};
    use daux_host_services::HostServices;
    use std::sync::Arc;

    /// A plug-in whose halves are irrelevant: these tests only exercise `create_editor`.
    struct EditorOnly {
        answer: fn() -> Option<Box<dyn Any>>,
        inner: Spy,
    }

    impl EditorOnly {
        fn new(answer: fn() -> Option<Box<dyn Any>>) -> Self {
            Self {
                answer,
                inner: Spy::new(Arc::new(Counts::default())),
            }
        }
    }

    impl DauxPlugin for EditorOnly {
        fn descriptor() -> PluginDescriptor {
            Spy::descriptor()
        }
        fn bus_layout(&self) -> BusLayout {
            BusLayout::stereo_effect()
        }
        fn processor(&mut self) -> &mut dyn DauxProcessor {
            self.inner.processor()
        }
        fn controller(&mut self) -> &mut dyn DauxController {
            self.inner.controller()
        }
        fn create_editor(&mut self) -> Option<Box<dyn Any>> {
            (self.answer)()
        }
    }

    fn open_and_close(mut graphic: Box<dyn DauxGraphic>) {
        let host = HostServices::null();
        let mut ctx = GraphicContext::new(
            WindowTarget::win32(0x1234).expect("a non-null hwnd is a valid target"),
            PhysicalSize::new(320, 240),
            ScaleFactor::ONE,
            SpyEditor::profile(),
            &host,
        );
        graphic.open(&mut ctx).expect("the spy editor opens");
        graphic.resize(PhysicalSize::new(640, 480)).unwrap();
        graphic.tick();
        graphic.close();
    }

    #[test]
    fn a_headless_plug_in_reports_no_editor_rather_than_an_error() {
        let mut plugin = EditorOnly::new(|| None);
        assert!(take_editor(&mut plugin).unwrap().is_none());
    }

    #[test]
    fn a_wrapped_editor_comes_back_usable() {
        let mut plugin = EditorOnly::new(|| editor(SpyEditor::default()));
        let graphic = take_editor(&mut plugin)
            .expect("a correctly wrapped editor")
            .expect("not headless");
        assert_eq!(graphic.descriptor().preferred_size.width, 320.0);
        open_and_close(graphic);
    }

    #[test]
    fn a_pre_erased_editor_comes_back_too() {
        let mut plugin = EditorOnly::new(|| {
            let chosen: Box<dyn DauxGraphic> = Box::new(SpyEditor::default());
            Some(boxed_editor(chosen))
        });
        let graphic = take_editor(&mut plugin).unwrap().expect("not headless");
        open_and_close(graphic);
    }

    /// The mistake this module exists to catch: boxing the concrete editor straight into the
    /// `Any`. `Box<dyn Any>` cannot be downcast to an unsized trait object, so the framework
    /// has no way to recover it — and must say so instead of panicking.
    #[test]
    fn boxing_the_editor_directly_is_a_clear_error_not_a_panic() {
        let mut plugin = EditorOnly::new(|| Some(Box::new(SpyEditor::default()) as Box<dyn Any>));
        let Err(err) = take_editor(&mut plugin) else {
            panic!("a directly boxed editor must not be accepted")
        };
        assert_eq!(err.kind(), ErrorKind::Plugin);
        assert_eq!(err.status_code(), daux_core::status::PLUGIN);
        assert!(
            err.message().contains("daux_plugin_api::editor"),
            "the error must name the fix: {err}"
        );
    }

    #[test]
    fn an_unrelated_value_is_refused_with_the_same_error() {
        for answer in [
            (|| Some(Box::new(42u32) as Box<dyn Any>)) as fn() -> Option<Box<dyn Any>>,
            || Some(Box::new(String::from("not an editor")) as Box<dyn Any>),
            || Some(Box::new(()) as Box<dyn Any>),
            // Double-boxed: a plausible mistake, and still not what we asked for.
            || Some(Box::new(Box::new(SpyEditor::default())) as Box<dyn Any>),
        ] {
            let mut plugin = EditorOnly::new(answer);
            let Err(err) = take_editor(&mut plugin) else {
                panic!("only a Box<dyn DauxGraphic> may be accepted")
            };
            assert_eq!(err.kind(), ErrorKind::Plugin);
        }
    }

    #[test]
    fn a_refused_value_is_dropped_rather_than_leaked() {
        /// Reports its own destruction.
        struct Noisy(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for Noisy {
            fn drop(&mut self) {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wrong: Box<dyn Any> = Box::new(Noisy(drops.clone()));
        let Err(err) = downcast_editor(wrong) else {
            panic!("a Noisy is not an editor")
        };
        assert_eq!(err.kind(), ErrorKind::Plugin);
        assert_eq!(
            drops.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the value we refused must still be destroyed"
        );
    }

    /// Rule 9: an editor may be created and destroyed many times over one plug-in's life,
    /// and each one is independent.
    #[test]
    fn editors_can_be_created_repeatedly_and_are_independent() {
        let mut plugin = EditorOnly::new(|| editor(SpyEditor::default()));
        for _ in 0..5 {
            let mut graphic = take_editor(&mut plugin).unwrap().expect("not headless");
            // A fresh editor every time — never a handle to the previous one.
            graphic.close();
            drop(graphic);
        }
    }

    #[test]
    fn the_wrapper_round_trips_without_going_through_a_plug_in() {
        let wrapped = editor(SpyEditor::default()).expect("editor() always yields Some");
        let graphic = downcast_editor(wrapped).unwrap();
        assert_eq!(
            graphic.capabilities(),
            graphic.descriptor().capabilities,
            "the recovered object is the real editor, defaults and all"
        );
    }

    /// The spy plug-in wires `create_editor` through [`editor`], so the whole path from
    /// `DauxPlugin` to a driveable editor is covered end to end.
    #[test]
    fn the_path_from_a_real_plug_in_ends_in_a_driveable_editor() {
        let counts = Arc::new(Counts::default());
        let mut plugin = Spy::new(counts.clone());
        let graphic = take_editor(&mut plugin).unwrap().expect("the spy has one");
        assert_eq!(counts.editors(), 1);
        open_and_close(graphic);
    }
}

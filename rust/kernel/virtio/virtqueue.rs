// SPDX-License-Identifier: GPL-2.0
// Author: Manos Pitsidianakis <manos@pitsidianak.is>

//! Virtqueue functionality.
//!
//! # Discovering virtqueues
//!
//! Inside your driver's [`kernel::virtio::Driver::probe`] method, call
//! [`kernel::virtio::Device::find_vqs`] method with your [`VirtqueueInfo`] struct.
//!
//! # Passing data to virtqueues
//!
//! Create your data as owned scatterlists with:
//!
//! - [`Virtqueue::new_readable_sg`] for data that can be read from the device, and
//! - [`Virtqueue::new_writable_sg`] for data that can be written from the device
//!
//! These methods will take ownership of your data so that you can add them to a virtqueue.
//!
//! To add the tables to the virtqueue, call one of the `add_*` methods (e.g.
//! [`Virtqueue::add_sgs`], [`Virtqueue::add_inbuf`] etc).
//!
//! Note that it is your responsibility to ensure the data is not dropped at least until the
//! virtqueue has marked it as used.
//!
//! # Used data buffer notifications
//!
//! The VIRTIO device will notify you of any used buffers by calling the
//! [`kernel::virtio::Driver::vq_callback`] method.
//!
//! You can access the buffer's token and amount of bytes written with the [`Virtqueue::get_buf`]
//! method.

use crate::{
    alloc::{
        Flags, //
    },
    bindings, //
    device::{
        Bound, //
    },
    error::{
        code::{
            ENOENT, //
        },
        to_result, //
        Result,    //
    },
    prelude::*, //
    str::{
        self, //
        CStr, //
    },
    sync::{
        Arc, //
    },
    types::Opaque, //
    virtio::{
        Device, //
        Driver, //
    },
};

use core::{
    ffi::c_uint,  //
    ptr::NonNull, //
};

/// Info for a virtqueue.
///
/// [`struct virtqueue_info`]: srctree/include/linux/virtio_config.h
#[doc(alias = "virtqueue_info")]
#[repr(transparent)]
pub struct VirtqueueInfo(Opaque<bindings::virtqueue_info>);

extern "C" fn vq_callback<T: Driver + 'static>(vq: *mut bindings::virtqueue) {
    // SAFETY: The kernel called this virtqueue callback and it must have provided a valid `vq`
    // pointer
    let vq = unsafe { Virtqueue::from_raw(vq) };
    let dev: &Device<Bound> = vq.dev().expect("Could not get device");
    let data = dev
        .as_ref()
        .drvdata::<T>()
        .expect("Could not borrow drvdata");
    T::vq_callback(&data, vq, dev);
}

impl VirtqueueInfo {
    #[inline]
    /// Create a new [`VirtqueueInfo`]
    pub const fn new<T: Driver + 'static>(name: &'static CStr, ctx: bool) -> Self {
        Self(Opaque::new(bindings::virtqueue_info {
            name: str::as_char_ptr_in_const_context(name),
            ctx,
            callback: Some(vq_callback::<T>),
        }))
    }
}

/// A container for discovered virtqueues returned by [`Device::find_vqs`] method.
///
/// This type can be indexed to receive a reference to a virtqueue.
///
/// It deletes the virtqueues when dropped.
pub struct Virtqueues {
    pub(super) inner: KVec<NonNull<Virtqueue>>,
}

// SAFETY: `bindings::virtqueue` is safe to be send to any task.
unsafe impl Send for Virtqueues {}

// SAFETY: `Virtqueues` has no interior mutability.
unsafe impl Sync for Virtqueues {}

impl Drop for Virtqueues {
    fn drop(&mut self) {
        let first_ref: &Virtqueue = &self[0];
        let Ok(vdev) = first_ref.dev() else {
            return;
        };
        vdev.reset();
        vdev.del_vqs();
    }
}

impl core::ops::Index<usize> for Virtqueues {
    type Output = Virtqueue;

    fn index(&self, index: usize) -> &Self::Output {
        // SAFETY: when this type was constructed, the values were promised to be valid pointers
        unsafe { self.inner[index].as_ref() }
    }
}

impl Virtqueues {
    #[allow(clippy::len_without_is_empty)]
    /// Returns the number of virtqueues.
    #[inline]
    pub const fn len(&self) -> usize {
        self.inner.len()
    }
}

/// An opaque handler for a virtqueue.
///
/// [`struct virtqueue`]: srctree/include/linux/virtio.h
#[repr(transparent)]
pub struct Virtqueue(Opaque<bindings::virtqueue>);

impl Virtqueue {
    /// Create a [`Virtqueue`] from a raw pointer.
    ///
    /// # Safety
    ///
    /// Callers must ensure that `ptr` is a properly initialized valid `virtqueue` pointer.
    #[inline]
    pub unsafe fn from_raw<'a>(ptr: *mut bindings::virtqueue) -> &'a Self {
        // SAFETY: The safety requirements of this function guarantee that `ptr` is a valid
        // pointer to a `struct virtqueue` for the duration of `'a`.
        unsafe { &*ptr.cast() }
    }

    /// Obtain the raw `struct virtqueue *`.
    #[inline]
    pub(crate) fn as_raw(&self) -> *mut bindings::virtqueue {
        self.0.get()
    }

    /// Get the [`Device`] associated with this virtqueue.
    #[inline]
    pub fn dev(&self) -> Result<&Device<Bound>> {
        // SAFETY: By the type invariants, `self.as_raw()` is a valid pointer to a `struct
        // virtqueue`.
        if unsafe { (*self.as_raw()).vdev }.is_null() {
            return Err(ENOENT);
        }
        // SAFETY: the pointer has been promised to be valid when self was created
        Ok(unsafe { &*(&*self.as_raw()).vdev.cast::<Device<Bound>>() })
    }

    /// Get the name of this virtqueue (mainly for debugging).
    #[inline]
    pub fn name(&self) -> Option<&CStr> {
        // SAFETY: the pointer has been promised to be valid when self was created
        let name_ptr = unsafe { (*self.as_raw()).name };
        if name_ptr.is_null() {
            return None;
        }
        // SAFETY: the name is promised to be a valid NUL-terminated string from the API contract.
        Some(unsafe { CStr::from_char_ptr(name_ptr) })
    }

    /// Get the zero-based ordinal number for this queue.
    #[inline]
    pub fn index(&self) -> c_uint {
        // SAFETY: the pointer has been promised to be valid when self was created
        unsafe { (*self.as_raw()).index }
    }

    /// Get the number of elements we expect to be able to fit.
    ///
    /// With indirect buffers, each buffer needs one element in the queue, otherwise a buffer will
    /// need one element per sg element.
    #[inline]
    pub fn num_free(&self) -> c_uint {
        // SAFETY: the pointer has been promised to be valid when self was created
        unsafe { (*self.as_raw()).num_free }
    }

    /// Get the maximum number of elements supported by the device.
    #[inline]
    pub fn num_max(&self) -> c_uint {
        // SAFETY: the pointer has been promised to be valid when self was created
        unsafe { (*self.as_raw()).num_max }
    }

    /// Get the vring size.
    #[inline]
    #[doc(alias = "virtqueue_get_vring_size")]
    pub fn vring_size(&self) -> u32 {
        // SAFETY: the pointer has been promised to be valid when self was created
        unsafe { bindings::virtqueue_get_vring_size(self.as_raw()) }
    }

    /// Notify virtqueue.
    #[inline]
    #[doc(alias = "virtqueue_notify")]
    pub fn notify(&self) -> bool {
        // SAFETY: the pointer has been promised to be valid when self was created
        unsafe { bindings::virtqueue_notify(self.as_raw()) }
    }

    /// Kick and prepare virtqueue.
    #[inline]
    #[doc(alias = "virtqueue_kick_prepare")]
    pub fn kick_prepare(&self) -> bool {
        // SAFETY: the pointer has been promised to be valid when self was created
        unsafe { bindings::virtqueue_kick_prepare(self.as_raw()) }
    }

    /// Kick virtqueue.
    #[inline]
    #[doc(alias = "virtqueue_kick")]
    pub fn kick(&self) -> bool {
        // SAFETY: the pointer has been promised to be valid when self was created
        unsafe { bindings::virtqueue_kick(self.as_raw()) }
    }

    /// Enable virtqueue's callback.
    #[inline]
    #[doc(alias = "virtqueue_enable_cb")]
    pub fn enable_cb(&self) -> bool {
        // SAFETY: the pointer has been promised to be valid when self was created
        unsafe { bindings::virtqueue_enable_cb(self.as_raw()) }
    }

    /// Disable virtqueue's callback.
    #[inline]
    #[doc(alias = "virtqueue_disable_cb")]
    pub fn disable_cb(&self) {
        // SAFETY: the pointer has been promised to be valid when self was created
        unsafe { bindings::virtqueue_disable_cb(self.as_raw()) }
    }

    /// Get a buffer from the virtqueue, if available.
    ///
    /// This method returns a pointer to the `token` value passed in [`Virtqueue::add_sgs`] method
    /// and the amount of bytes that were written by the device.
    #[inline]
    #[doc(alias = "virtqueue_get_buf")]
    pub fn get_buf(&'_ self) -> Option<(NonNull<u8>, u32)> {
        let mut len = 0;
        // SAFETY: the pointer has been promised to be valid when self was created
        let ptr = unsafe { bindings::virtqueue_get_buf(self.as_raw(), &mut len) };
        Some((NonNull::new(ptr.cast())?, len))
    }

    /// Add a list of scatter-gather lists to virtqueue.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the buffers and token live at least as long until the virtqueue
    /// has mark them used.
    #[inline]
    #[doc(alias = "virtqueue_add_sgs")]
    pub unsafe fn add_sgs<'token, PIn, POut, Token>(
        &'_ self,
        out_sgs: &'token SGReadable<POut>,
        in_sgs: &'token SGWritable<PIn>,
        token: Pin<&'token Token>,
        gfp: Flags,
    ) -> Result
    where
        PIn: VirtqueueMappable + 'static,
        POut: VirtqueueMappable + 'static,
    {
        let mut sgs = KVec::with_capacity(2, gfp)?;

        sgs.push(out_sgs.inner.inner.get(), gfp)?;
        let out_sgs_num = 1;
        sgs.push(in_sgs.inner.inner.get(), gfp)?;
        let in_sgs_num = 1;

        // SAFETY: `self` has been promised to be valid when self was created
        to_result(unsafe {
            bindings::virtqueue_add_sgs(
                self.as_raw(),
                sgs.as_mut_ptr(),
                out_sgs_num,
                in_sgs_num,
                NonNull::new(core::ptr::from_ref::<Token>(&*token.as_ref()).cast_mut())
                    .unwrap()
                    .as_ptr()
                    .cast(),
                gfp.as_raw(),
            )
        })
    }

    /// Add a device-writable scatter-gather list to the virtqueue.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the buffer and token live at least as long until the virtqueue
    /// has mark them used.
    #[inline]
    #[doc(alias = "virtqueue_add_inbuf")]
    pub unsafe fn add_inbuf<'token, PIn, Token>(
        &'_ self,
        in_sgs: &'token SGWritable<PIn>,
        token: Pin<&'token Token>,
        gfp: Flags,
    ) -> Result
    where
        PIn: VirtqueueMappable + 'static,
    {
        // SAFETY: `self` has been promised to be valid when self was created
        to_result(unsafe {
            bindings::virtqueue_add_inbuf(
                self.as_raw(),
                in_sgs.inner.inner.get(),
                1,
                NonNull::new(core::ptr::from_ref::<Token>(&*token.as_ref()).cast_mut())
                    .unwrap()
                    .as_ptr()
                    .cast(),
                gfp.as_raw(),
            )
        })
    }

    /// Create a scatter-gather table readable by the device.
    #[inline]
    pub fn new_readable_sg<P>(&self, buf: P, gfp: Flags) -> Result<SGReadable<P>>
    where
        P: VirtqueueMappable + 'static,
    {
        Ok(SGReadable {
            inner: Arc::pin_init(SGInner::new(buf), gfp)?,
        })
    }

    /// Create a scatter-gather table writable by the device.
    #[inline]
    pub fn new_writable_sg<P>(&self, buf: P, gfp: Flags) -> Result<SGWritable<P>>
    where
        P: VirtqueueMappable + 'static,
    {
        Ok(SGWritable {
            inner: Arc::pin_init(SGInner::new(buf), gfp)?,
        })
    }
}

/// Trait to be implemented by types which can be mapped as buffers in virtqueues.
///
/// # Safety
///
/// The trait must be implemented for contiguously allocated types.
///
/// It is the user's responsibility that the data lives as long as it is mapped in virtqueues.
pub unsafe trait VirtqueueMappable {
    /// The type of the allocation, e.g. a `Box<T>` allocation has a `T` target.
    type Target: Sized;

    /// Return a pointer to the base address.
    fn data(&self) -> *const Self::Target;

    /// Return the allocation size.
    fn size(&self) -> usize;
}

#[pin_data]
struct SGInner<P> {
    #[pin]
    inner: Opaque<bindings::scatterlist>,
    buf: P,
}

// SAFETY: `SGInner` is safe to be sent to any task.
unsafe impl<P: Send> Send for SGInner<P> {}

// SAFETY: `SGInner` has no interior mutability.
unsafe impl<P: Sync> Sync for SGInner<P> {}

impl<P: VirtqueueMappable> SGInner<P> {
    fn new(buf: P) -> impl PinInit<Self, Error> {
        pin_init!(Self {
            inner: {
                let sg = Opaque::zeroed();
                let data = buf.data();
                if data.is_null() {
                    return Err(EINVAL);
                }
                let size = u32::try_from(buf.size()).map_err(|_| EINVAL)?;
                // SAFETY: `sg` is a valid scattergather pointer.
                unsafe { bindings::sg_init_one(sg.get(), data.cast(), size) };
                sg
            },
            buf
        }? Error)
    }
}

/// A scatterlist that will be DMA-mapped as device-readable.
///
/// Created by [`Virtqueue::new_readable_sg`].
#[derive(Clone)]
pub struct SGReadable<P> {
    inner: Arc<SGInner<P>>,
}

/// A scatterlist that will be DMA-mapped as device-writable.
///
/// Created by [`Virtqueue::new_writable_sg`].
pub struct SGWritable<P> {
    inner: Arc<SGInner<P>>,
}

impl<P> SGWritable<P>
where
    P: VirtqueueMappable,
{
    /// Get a read-only pointer to the sg's data.
    ///
    /// # Safety
    ///
    /// The data might be DMA'ed by the device at any point, it is up to the caller to ensure it
    /// does not read from this reference if that can happen.
    #[inline]
    pub unsafe fn data(&self) -> Option<*const <P as VirtqueueMappable>::Target> {
        let ptr = self.inner.buf.data();
        if ptr.is_null() {
            return None;
        }
        Some(ptr)
    }
}

// SPDX-License-Identifier: GPL-2.0
// Author: Manos Pitsidianakis <manos@pitsidianak.is>

//! VIRTIO abstraction.
//!
//! To implement a VIRTIO driver:
//!
//! - Implement the [`Driver`] trait for your driver type (use
//!   [`virtio_device_table`](kernel::virtio_device_table) macro to declare the `ID_TABLE`
//!   associated item)
//! - Use the [`module_virtio_driver`](kernel::module_virtio_driver) macro to declare your module.
//!
//! # Interacting with virtqueues
//!
//! See [`virtqueue`] documentation.

use crate::{
    alloc::{
        Flags, //
    },
    bindings,               //
    device_id::RawDeviceId, //
    error::{
        from_result,          //
        to_result,            //
        Error,                //
        Result,               //
        VTABLE_DEFAULT_ERROR, //
    },
    ffi::c_uint,   //
    prelude::*,    //
    types::Opaque, //
};

use core::{
    marker::PhantomData, //
    pin::Pin,            //
    ptr::NonNull,        //
};

pub mod utils;
pub mod virtqueue;

/// IdTable type for virtio drivers.
pub type IdTable<T> = &'static dyn crate::device_id::IdTable<DeviceId, T>;

/// A VIRTIO device id.
///
/// [`struct virtio_device_id`]: srctree/include/linux/mod_devicetable.h
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct DeviceId(bindings::virtio_device_id);

// SAFETY: `DeviceId` is a `#[repr(transparent)]` wrapper of `struct virtio_device_id` and
// does not add additional invariants, so it's safe to transmute to `RawType`.
unsafe impl RawDeviceId for DeviceId {
    type RawType = bindings::virtio_device_id;
}

impl DeviceId {
    #[inline]
    /// Create a new device id
    pub const fn new(device: VirtioID) -> Self {
        Self::new_with_vendor(device, VIRTIO_DEV_ANY_ID)
    }

    #[inline]
    /// Create a new device id with vendor
    pub const fn new_with_vendor(device: VirtioID, vendor: u32) -> Self {
        // TODO: Replace with `bindings::virtio_device_id::default()` once stabilized for `const`.
        // SAFETY: FFI type is valid to be zero-initialized.
        let mut ret: bindings::virtio_device_id = unsafe { core::mem::zeroed() };
        ret.device = device as u32;
        ret.vendor = vendor;
        Self(ret)
    }
}

/// Create a virtio `IdArray` with its alias for modpost.
#[macro_export]
macro_rules! virtio_device_table {
    ($table_name:ident, $module_table_name:ident, $id_info_type: ty, $table_data:expr) => {
        const $table_name: $crate::device_id::IdArray<
            $crate::virtio::DeviceId,
            $id_info_type,
            { $table_data.len() },
        > = $crate::device_id::IdArray::new_without_index($table_data);

        $crate::module_device_table!("virtio", $module_table_name, $table_name);
    };
}

/// Declares a kernel module that exposes a single virtio driver.
#[macro_export]
macro_rules! module_virtio_driver {
    ($($f:tt)*) => {
        $crate::module_driver!(<T>, $crate::virtio::Adapter<T>, { $($f)* });
    };
}

/// The Virtio driver trait.
///
/// Drivers must implement this trait in order to get a virtio driver registered.
#[vtable]
pub trait Driver: Send + Sync {
    /// The type holding information about each device id supported by the driver.
    // TODO: Use `associated_type_defaults` once stabilized:
    //
    // ```
    // type IdInfo: 'static = ();
    // ```
    type IdInfo: 'static;

    /// The table of device ids supported by the driver.
    const ID_TABLE: IdTable<Self::IdInfo>;

    /// virtio driver probe.
    ///
    /// Called when a new virtio device is added or discovered. Implementers should
    /// attempt to initialize the device here, but should not sleep since driver data is set
    /// after this method returns successfully.
    fn probe(dev: &Device<crate::device::Core>) -> impl PinInit<Self, Error>;

    /// virtio driver init.
    ///
    /// The device will be set to ready before this function is called.
    /// Called after a virtio device is probed successfully, can sleep.
    /// If an error is returned, the driver will fail to register.
    fn init(&self, dev: &Device<crate::device::Bound>) -> Result;

    #[inline]
    /// Optional function to validate features and config space before probing.
    fn validate(_: &Device<crate::device::Core>) -> Result {
        build_error!(VTABLE_DEFAULT_ERROR)
    }

    /// Method called after a successful probe and initialization.
    #[inline]
    fn scan(&self, _: &Device<crate::device::Bound>) {
        build_error!(VTABLE_DEFAULT_ERROR)
    }

    /// Optional function to call when the device configuration changes; may be called in interrupt
    /// context.
    #[inline]
    fn config_changed(&self, _: &Device<crate::device::Bound>) {
        build_error!(VTABLE_DEFAULT_ERROR)
    }

    // TODO: add freeze/restore callbacks.

    /// Optional function to call when a transport specific reset occurs.
    #[inline]
    fn reset_prepare(&self, _: &Device<crate::device::Bound>) -> Result {
        build_error!(VTABLE_DEFAULT_ERROR)
    }

    /// Optional function to call after transport specific reset operation has finished.
    #[inline]
    fn reset_done(&self, _: &Device<crate::device::Bound>) -> Result {
        build_error!(VTABLE_DEFAULT_ERROR)
    }

    /// Optional function to synchronize with the device on shutdown. If provided, replaces the
    /// virtio core implementation.
    #[inline]
    fn shutdown(&self, _: &Device<crate::device::Bound>) {
        build_error!(VTABLE_DEFAULT_ERROR)
    }

    /// Method called when buffers are consumed.
    ///
    /// Users can identify the virtqueue by calling [`virtqueue::Virtqueue::index`].
    ///
    /// The consumed buffer can be retrieved with [`virtqueue::Virtqueue::get_buf`].
    fn vq_callback(&self, vq: &virtqueue::Virtqueue, dev: &Device<crate::device::Bound>);

    /// virtio driver remove.
    ///
    /// Called when a [`Device`] is removed from its [`Driver`]. Implementing this callback
    /// is optional.
    ///
    /// This callback serves as a place for drivers to perform teardown operations that require a
    /// `&Device<Core>` or `&Device<Bound>` reference. For instance, drivers may try to perform I/O
    /// operations to gracefully tear down the device.
    ///
    /// Otherwise, release operations for driver resources should be performed in `Self::drop`.
    fn remove(dev: &Device<crate::device::Core>, this: Pin<&Self>) {
        _ = (dev, this);
    }
}

/// Abstraction for the virtio device structure (`struct virtio_device`).
///
/// [`struct virtio_device`]: srctree/include/linux/virtio.h
#[repr(transparent)]
pub struct Device<Ctx: crate::device::DeviceContext = crate::device::Normal>(
    Opaque<bindings::virtio_device>,
    PhantomData<Ctx>,
);

impl<Ctx: crate::device::DeviceContext> Device<Ctx> {
    #[inline]
    fn as_raw(&self) -> *mut bindings::virtio_device {
        self.0.get()
    }
}

// SAFETY: `virtio::Device` is a transparent wrapper of `struct virtio_device`.
// The offset is guaranteed to point to a valid device field inside `virtio::Device`.
unsafe impl<Ctx: crate::device::DeviceContext> crate::device::AsBusDevice<Ctx> for Device<Ctx> {
    const OFFSET: usize = core::mem::offset_of!(bindings::virtio_device, dev);
}

// SAFETY: `Device` is a transparent wrapper of a type that doesn't depend on `Device`'s generic
// argument.
kernel::impl_device_context_deref!(unsafe { Device });

macro_rules! __impl_device_context_into_aref {
    ($src:ty) => {
        impl ::core::convert::From<&$crate::virtio::Device<$src>>
            for crate::sync::aref::ARef<crate::device::Device>
        {
            fn from(dev: &$crate::virtio::Device<$src>) -> Self {
                (&**dev.as_ref()).into()
            }
        }
    };
}

__impl_device_context_into_aref!(crate::device::CoreInternal);
__impl_device_context_into_aref!(crate::device::Core);
__impl_device_context_into_aref!(crate::device::Bound);

impl Device<crate::device::Bound> {
    /// Returns the virtio device ID.
    ///
    /// If the virtio ID value is not recognized, it is returned as an `Err` variant.
    #[inline]
    pub fn device_id(&self) -> Result<VirtioID, u32> {
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct virtio_device`.
        VirtioID::try_from(unsafe { (*self.as_raw()).id.device })
    }

    /// Returns the virtio vendor ID.
    #[inline]
    pub fn vendor_id(&self) -> u32 {
        // SAFETY: `self.as_raw` is a valid pointer to a `struct virtio_device`.
        unsafe { (*self.as_raw()).id.vendor }
    }

    /// Reset device.
    #[doc(alias = "virtio_reset_device")]
    #[inline]
    fn reset(&self) {
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct virtio_device`.
        unsafe { bindings::virtio_reset_device(self.as_raw()) }
    }

    /// Mark device as ready.
    ///
    /// This method should only be called by the [`kernel::virtio`] wrapper internals, not the
    /// driver, after the driver's probe method returns. The reason is that after setting the
    /// device as ready, the virtqueue callbacks may fire, and if we have not called `set_drvdata`
    /// yet, there is no way for the callback to access the driver data.
    #[doc(alias = "virtio_device_ready")]
    #[inline]
    fn ready(&self) {
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct virtio_device`.
        unsafe { bindings::virtio_device_ready(self.as_raw()) }
    }

    /// Return virtqueues for this device.
    ///
    /// When the returned value is dropped, the virtqueues will be deleted.
    #[doc(alias = "virtio_find_vqs")]
    pub fn find_vqs<T: Driver>(
        &self,
        info: &[virtqueue::VirtqueueInfo],
        gfp: Flags,
    ) -> Result<virtqueue::Virtqueues> {
        if info.is_empty() {
            return Err(EINVAL);
        }
        let mut vqs = KVec::with_capacity(info.len(), gfp)?;
        let mut inner = KVec::with_capacity(info.len(), gfp)?;
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct virtio_device`.
        to_result(unsafe {
            bindings::virtio_find_vqs(
                self.as_raw(),
                info.len().try_into()?,
                vqs.spare_capacity_mut().as_mut_ptr().cast(),
                info.as_ptr().cast_mut().cast(),
                core::ptr::null_mut(),
            )
        })?;
        // SAFETY: virtio_find_vqs returned successfully so `vqs` must be populated.
        unsafe { vqs.inc_len(info.len()) };
        for vq in vqs {
            if let Err(err) = NonNull::new(vq)
                .ok_or(EINVAL)
                .and_then(|vq| Ok(inner.push(vq, gfp)?))
            {
                self.del_vqs();
                return Err(err);
            }
        }
        Ok(virtqueue::Virtqueues { inner })
    }

    /// Delete virtqueues from this device.
    pub(crate) fn del_vqs(&self) {
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct virtio_device`.
        let config = unsafe { (*self.as_raw()).config };
        // SAFETY: `config` points to a valid virtqueue config struct.
        if let Some(del_vqs) = unsafe { (*config).del_vqs } {
            // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
            // `struct virtio_device`.
            unsafe { del_vqs(self.as_raw()) }
        }
    }

    /// Checks if the device has a feature bit.
    #[inline]
    pub fn has_feature(&self, fbit: c_uint) -> bool {
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct virtio_device`.
        unsafe { bindings::virtio_has_feature(self.as_raw(), fbit) }
    }

    /// Returns unique device index on the virtio bus.
    #[inline]
    pub fn index(&self) -> c_int {
        // SAFETY: By its type invariant `self.as_raw` is always a valid pointer to a
        // `struct virtio_device`.
        unsafe { (*self.as_raw()).index }
    }
}

impl<Ctx: crate::device::DeviceContext> AsRef<crate::device::Device<Ctx>> for Device<Ctx> {
    #[inline]
    fn as_ref(&self) -> &crate::device::Device<Ctx> {
        // SAFETY: By the type invariant of `Self`, `self.as_raw()` is a pointer to a valid
        // `struct virtio_device`.
        let dev = unsafe { core::ptr::addr_of_mut!((*self.as_raw()).dev) };

        // SAFETY: `dev` points to a valid `struct device`.
        unsafe { crate::device::Device::from_raw(dev) }
    }
}

/// An adapter for the registration of virtio drivers.
pub struct Adapter<T: Driver>(T);

// SAFETY:
// - `bindings::virtio_driver` is a C type declared as `repr(C)`.
// - `T` is the type of the driver's device private data.
// - `struct virtio_driver` embeds a `struct device_driver`.
// - `DEVICE_DRIVER_OFFSET` is the correct byte offset to the embedded `struct device_driver`.
unsafe impl<T: Driver + 'static> crate::driver::DriverLayout for Adapter<T> {
    type DriverType = bindings::virtio_driver;
    type DriverData = T;
    const DEVICE_DRIVER_OFFSET: usize = core::mem::offset_of!(Self::DriverType, driver);
}

// SAFETY: A call to `unregister` for a given instance of `DriverType` is guaranteed to be valid if
// a preceding call to `register` has been successful.
unsafe impl<T: Driver + 'static> crate::driver::RegistrationOps for Adapter<T> {
    unsafe fn register(
        vdrv: &Opaque<Self::DriverType>,
        name: &'static CStr,
        module: &'static ThisModule,
    ) -> Result {
        // SAFETY: It's safe to set the fields of `struct virtio_driver` on initialization.
        unsafe {
            (*vdrv.get()).driver.name = name.as_char_ptr();
            (*vdrv.get()).id_table = T::ID_TABLE.as_ptr();
            (*vdrv.get()).probe = Some(Self::probe_callback);
            (*vdrv.get()).remove = Some(Self::remove_callback);
            if <T as Driver>::HAS_VALIDATE {
                (*vdrv.get()).validate = Some(Self::validate_callback);
            }
            if <T as Driver>::HAS_SCAN {
                (*vdrv.get()).scan = Some(Self::scan_callback);
            }
            if <T as Driver>::HAS_CONFIG_CHANGED {
                (*vdrv.get()).config_changed = Some(Self::config_changed_callback);
            }
            if <T as Driver>::HAS_RESET_PREPARE {
                (*vdrv.get()).reset_prepare = Some(Self::reset_prepare_callback);
            }
            if <T as Driver>::HAS_RESET_DONE {
                (*vdrv.get()).reset_done = Some(Self::reset_done_callback);
            }
            if <T as Driver>::HAS_SHUTDOWN {
                (*vdrv.get()).shutdown = Some(Self::shutdown_callback);
            }
        }

        // SAFETY: `vdrv` is guaranteed to be a valid `DriverType`.
        to_result(unsafe { bindings::__register_virtio_driver(vdrv.get(), module.0) })
    }

    unsafe fn unregister(vdrv: &Opaque<Self::DriverType>) {
        // SAFETY: `vdrv` is guaranteed to be a valid `DriverType`.
        unsafe { bindings::unregister_virtio_driver(vdrv.get()) }
    }
}

// Helper macro to define C callbacks for optional driver methods.
macro_rules! marshal_optional_cb {
    (void $c_method:ident => $rs_method:ident) => {
        extern "C" fn $c_method(vdev: *mut bindings::virtio_device) {
            // SAFETY: The kernel only ever calls the callback with a valid pointer to a `struct
            // virtio_device`.
            //
            // INVARIANT: `vdev` is valid for the duration of this callback.
            let dev = unsafe { &*vdev.cast::<Device<crate::device::CoreInternal>>() };

            // SAFETY: this callback is only ever called after a successful call to
            // `probe_callback`, hence it's guaranteed that `Device::set_drvdata()` has been called
            // and stored a `Pin<KBox<T>>`.
            let data = unsafe { dev.as_ref().drvdata_borrow::<T>() };
            T::$rs_method(&data, dev);
        }
    };
    (int $c_method:ident => $rs_method:ident) => {
        extern "C" fn $c_method(vdev: *mut bindings::virtio_device) -> c_int {
            // SAFETY: The kernel only ever calls the callback with a valid pointer to a `struct
            // virtio_device`.
            //
            // INVARIANT: `vdev` is valid for the duration of this callback.
            let dev = unsafe { &*vdev.cast::<Device<crate::device::CoreInternal>>() };

            // SAFETY: this callback is only ever called after a successful call to
            // `probe_callback`, hence it's guaranteed that `Device::set_drvdata()` has been called
            // and stored a `Pin<KBox<T>>`.
            let data = unsafe { dev.as_ref().drvdata_borrow::<T>() };
            from_result(|| {
                T::$rs_method(&data, dev)?;
                Ok(0)
            })
        }
    };
    ($($ret:tt $c_method:ident => $rs_method:ident),*$(,)?) => {
        $(marshal_optional_cb!{$ret $c_method => $rs_method})*
    };
}

impl<T: Driver + 'static> Adapter<T> {
    extern "C" fn probe_callback(vdev: *mut bindings::virtio_device) -> c_int {
        // SAFETY: The kernel only ever calls the probe callback with a valid pointer to a `struct
        // virtio_device`.
        //
        // INVARIANT: `vdev` is valid for the duration of `probe_callback()`.
        let dev = unsafe { &*vdev.cast::<Device<crate::device::CoreInternal>>() };
        from_result(|| {
            let data = T::probe(dev);

            dev.as_ref().set_drvdata(data)?;
            // SAFETY: `Device::set_drvdata()` was just called so it's safe to borrow the data.
            let data = unsafe { dev.as_ref().drvdata_borrow::<T>() };
            dev.ready();
            if let Err(err) = T::init(&data, dev) {
                T::remove(dev, data);
                dev.reset();
                // SAFETY: `Device::set_drvdata()` was just called so it's safe to re-obtain the
                // data.
                let data = unsafe { dev.as_ref().drvdata_obtain::<T>() }.unwrap();
                drop(data);
                return Err(err);
            }
            Ok(0)
        })
    }

    extern "C" fn remove_callback(vdev: *mut bindings::virtio_device) {
        // SAFETY: The kernel only ever calls the remove callback with a valid pointer to a `struct
        // virtio_device`.
        //
        // INVARIANT: `vdev` is valid for the duration of `remove_callback()`.
        let dev = unsafe { &*vdev.cast::<Device<crate::device::CoreInternal>>() };

        // SAFETY: `remove_callback` is only ever called after a successful call to
        // `probe_callback`, hence it's guaranteed that `Device::set_drvdata()` has been called
        // and stored a `Pin<KBox<T>>`.
        let data = unsafe { dev.as_ref().drvdata_borrow::<T>() };

        T::remove(dev, data);
        dev.reset();
    }

    extern "C" fn validate_callback(vdev: *mut bindings::virtio_device) -> c_int {
        // SAFETY: The kernel only ever calls the callback with a valid pointer to a `struct
        // virtio_device`.
        //
        // INVARIANT: `vdev` is valid for the duration of this callback.
        let dev = unsafe { &*vdev.cast::<Device<crate::device::CoreInternal>>() };
        from_result(|| {
            T::validate(dev)?;
            Ok(0)
        })
    }

    marshal_optional_cb! {
        void scan_callback => scan,
        void config_changed_callback => config_changed,
        int reset_prepare_callback => reset_prepare,
        int reset_done_callback => reset_done,
        void shutdown_callback => shutdown,
    }
}

/// Any vendor
pub const VIRTIO_DEV_ANY_ID: u32 = 0xffffffff;

// Helper macro to define both VirtioID enum and its TryFrom impl at the same time.
macro_rules! def_virtio_id {
    ($($(#[$($doc:tt)*])*
       $name:ident = $value:ident
    ),*$(,)?) => {
        /// Virtio IDs
        ///
        /// C header: [`include/uapi/linux/virtio_ids.h`](srctree/include/uapi/linux/virtio_ids.h)
        #[repr(u32)]
        #[derive(Copy, Clone, Eq, Debug, PartialEq, Hash, PartialOrd, Ord)]
        pub enum VirtioID {
            $(
                $(#[$($doc)*])* $name = bindings::$value
            ),*
        }

        impl TryFrom<u32> for VirtioID {
            type Error = u32;

            fn try_from(value: u32) -> Result<Self, Self::Error> {
                match value {
                    $(
                        v if v == bindings::$value => Ok(Self::$name),
                    )*
                    other => Err(other)
                }
            }
        }

    };
}

def_virtio_id! {
    /// virtio net
    Net = VIRTIO_ID_NET,
    /// virtio block
    Block = VIRTIO_ID_BLOCK,
    /// virtio console
    Console = VIRTIO_ID_CONSOLE,
    /// virtio rng
    Rng = VIRTIO_ID_RNG,
    /// virtio balloon
    Balloon = VIRTIO_ID_BALLOON,
    /// virtio ioMemory
    IOMem = VIRTIO_ID_IOMEM,
    /// virtio remote processor messaging
    RPMSG = VIRTIO_ID_RPMSG,
    /// virtio scsi
    Scsi = VIRTIO_ID_SCSI,
    /// 9p virtio console
    NineP = VIRTIO_ID_9P,
    /// virtio WLAN MAC
    Mac80211Wlan = VIRTIO_ID_MAC80211_WLAN,
    /// virtio remoteproc serial link
    RPROCSerial = VIRTIO_ID_RPROC_SERIAL,
    /// Virtio caif
    CAIF = VIRTIO_ID_CAIF,
    /// virtio memory balloon
    MemoryBalloon = VIRTIO_ID_MEMORY_BALLOON,
    /// virtio GPU
    GPU = VIRTIO_ID_GPU,
    /// virtio clock/timer
    Clock = VIRTIO_ID_CLOCK,
    /// virtio input
    Input = VIRTIO_ID_INPUT,
    /// virtio vsock transport
    VSock = VIRTIO_ID_VSOCK,
    /// virtio crypto
    Crypto = VIRTIO_ID_CRYPTO,
    /// virtio signal distribution device
    SignalDist = VIRTIO_ID_SIGNAL_DIST,
    /// virtio pstore device
    Pstore = VIRTIO_ID_PSTORE,
    /// virtio IOMMU
    Iommu = VIRTIO_ID_IOMMU,
    /// virtio mem
    Mem = VIRTIO_ID_MEM,
    /// virtio sound
    Sound = VIRTIO_ID_SOUND,
    /// virtio filesystem
    FS = VIRTIO_ID_FS,
    /// virtio pmem
    PMem = VIRTIO_ID_PMEM,
    /// virtio rpmb
    RPMB = VIRTIO_ID_RPMB,
    /// virtio mac80211-hwsim
    Mac80211Hwsim = VIRTIO_ID_MAC80211_HWSIM,
    /// virtio video encoder
    VideoEncoder = VIRTIO_ID_VIDEO_ENCODER,
    /// virtio video decoder
    VideoDecoder = VIRTIO_ID_VIDEO_DECODER,
    /// virtio SCMI
    SCMI = VIRTIO_ID_SCMI,
    /// virtio nitro secure module
    NitroSecMod = VIRTIO_ID_NITRO_SEC_MOD,
    /// virtio i2c adapter
    I2CAdapter = VIRTIO_ID_I2C_ADAPTER,
    /// virtio watchdog
    Watchdog = VIRTIO_ID_WATCHDOG,
    /// virtio can
    CAN = VIRTIO_ID_CAN,
    /// virtio dmabuf
    DMABuf = VIRTIO_ID_DMABUF,
    /// virtio parameter server
    ParamServ = VIRTIO_ID_PARAM_SERV,
    /// virtio audio policy
    AudioPolicy = VIRTIO_ID_AUDIO_POLICY,
    /// virtio bluetooth
    BT = VIRTIO_ID_BT,
    /// virtio gpio
    GPIO = VIRTIO_ID_GPIO,
    /// virtio spi
    SPI = VIRTIO_ID_SPI,
}

// SPDX-License-Identifier: GPL-2.0
// Author: Manos Pitsidianakis <manos@pitsidianak.is>

//! Virtio entropy driver

// TODO: use irq-disabling spinlocks when supported

use core::{
    ptr::slice_from_raw_parts, //
    sync::atomic::{
        AtomicBool, //
        AtomicU32,  //
        Ordering,   //
    },
};

use kernel::{
    device::{
        Bound, //
        Core,  //
    },
    hw_random::{
        Buffer,    //
        HwRng,     //
        HwRngImpl, //
    },
    prelude::*,       //
    str::CString,     //
    sync::Completion, //
    virtio::{
        self,         //
        virtqueue::*, //
    },
};

#[vtable]
impl HwRngImpl for VirtioRngInner {
    fn cleanup(&self) {
        self.have_data.complete();
    }

    fn read(&self, data: &mut Buffer<'_>, can_wait: bool) -> Result<()> {
        if self.removed.load(Ordering::Acquire) {
            return Err(ENODEV);
        }
        self.copy_data(data)?;
        if !can_wait {
            return Ok(());
        }
        while !data.is_empty() {
            // data_avail is 0 but a request is pending
            self.have_data.wait_for_completion_killable()?;
            if self.removed.load(Ordering::Acquire) || self.data_avail.load(Ordering::Acquire) == 0
            {
                break;
            }
            self.copy_data(data)?;
        }
        Ok(())
    }
}

const DATA_BUF_SIZE: usize = 64;

#[pin_data]
struct VirtioRngInner {
    vq: Virtqueues,
    token: Pin<KBox<()>>,
    #[pin]
    sg: SGWritable<KBox<[u8; DATA_BUF_SIZE]>>,
    #[pin]
    have_data: Completion,
    data_idx: AtomicU32,
    data_avail: AtomicU32,
    removed: AtomicBool,
    in_flight: AtomicBool,
}

impl VirtioRngInner {
    fn new(name: CString, vq: Virtqueues) -> impl PinInit<HwRng<Self>, Error> {
        HwRng::new(
            name,
            0,
            pin_init!(Self {
                sg <- vq[0].new_writable_sg(
                          KBox::new([0_u8; DATA_BUF_SIZE], GFP_KERNEL)?,
                          GFP_KERNEL
                      ),
                token: KBox::pin_init((), GFP_KERNEL)?,
                have_data <- Completion::new(),
                data_idx: AtomicU32::new(0),
                data_avail: AtomicU32::new(0),
                removed: AtomicBool::new(false),
                in_flight: AtomicBool::new(false),
                vq,
            }? Error),
        )
    }

    fn request_entropy(&self) -> Result {
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        if self.removed.load(Ordering::Acquire) {
            return Err(ENODEV);
        }
        self.have_data.reinit();
        self.data_idx.store(0, Ordering::Release);
        // SAFETY: `self.sg` lives as long as the device
        if let Err(err) = unsafe { self.vq[0].add_inbuf(&self.sg, self.token.as_ref(), GFP_KERNEL) }
        {
            self.in_flight.store(false, Ordering::Release);
            return Err(err);
        }
        self.vq[0].kick();
        Ok(())
    }

    fn copy_data(&self, buf: &mut Buffer<'_>) -> Result<()> {
        let avail = self.data_avail.load(Ordering::Acquire);
        if avail == 0 {
            self.request_entropy()?;
            return Ok(());
        }
        let size = buf.len().min(avail as usize).min(DATA_BUF_SIZE);

        // SAFETY: We have available data
        let data: *const [u8; DATA_BUF_SIZE] = unsafe { self.sg.data().ok_or(EIO)? };
        // SAFETY: T and MaybeUninit<T> have the same layout
        let source_data: &[u8] = unsafe {
            &*slice_from_raw_parts(
                data.cast::<u8>()
                    .add(self.data_idx.fetch_add(size as u32, Ordering::Acquire) as usize),
                size,
            )
        };
        buf.write(source_data);
        let Ok(left) = self.data_avail.compare_exchange(
            avail,
            avail - (size as u32),
            Ordering::SeqCst,
            Ordering::Acquire,
        ) else {
            return Ok(());
        };
        if left == size as u32 {
            self.request_entropy()?;
        }
        Ok(())
    }
}

#[pin_data]
struct VirtioRngDriver {
    inner: Pin<KBox<HwRng<VirtioRngInner>>>,
}

#[vtable]
impl virtio::Driver for VirtioRngDriver {
    type IdInfo = ();

    /// The table of device ids supported by the driver.
    const ID_TABLE: virtio::IdTable<Self::IdInfo> = &VIRTIO_RNG_TABLE;

    fn probe(vdev: &virtio::Device<Core>) -> impl PinInit<Self, Error> {
        let vqs_info: [VirtqueueInfo; 1] = [VirtqueueInfo::new::<Self>(c"requestq", false)];
        try_pin_init!(Self {
            inner: {
                let vqs = match vdev.find_vqs::<Self>(&vqs_info, GFP_KERNEL) {
                    Ok(vqs) => vqs,
                    Err(err) => {
                        pr_err!("Could not find vqs: {err:?}.\n");
                        return Err(err);
                    }
                };
                let name = CString::try_from_fmt(fmt!("virtio_rng.{}", vdev.index()))?;
                KBox::pin_init(VirtioRngInner::new(name, vqs), GFP_KERNEL)?
            },
        })
    }

    fn init(&self, _: &virtio::Device<Bound>) -> Result {
        self.inner.request_entropy()?;
        Ok(())
    }

    fn scan(&self, _: &virtio::Device<Bound>) {
        if let Err(err) = self.inner.register() {
            pr_err!("Could not register hwrng: {err:?}.\n");
        }
    }

    fn vq_callback(&self, vq: &Virtqueue, _: &virtio::Device<Bound>) {
        if let Some((_, len)) = vq.get_buf() {
            self.inner
                .data_avail
                .store(len.min(DATA_BUF_SIZE as u32), Ordering::Release);
            self.inner.in_flight.store(false, Ordering::Release);
            self.inner.have_data.complete();
        }
    }

    fn remove(_: &virtio::Device<Core>, this: Pin<&Self>) {
        this.inner.removed.store(true, Ordering::Release);
        this.inner.data_avail.store(0, Ordering::Release);
        this.inner.have_data.complete_all();
        this.inner.unregister();
    }
}

kernel::virtio_device_table!(
    VIRTIO_RNG_TABLE,
    MODULE_VIRTIO_RNG_TABLE,
    <VirtioRngDriver as virtio::Driver>::IdInfo,
    [(virtio::DeviceId::new(virtio::VirtioID::Rng), ())]
);

kernel::module_virtio_driver! {
    type: VirtioRngDriver,
    name: "virtio_rng",
    authors: ["Manos Pitsidianakis"],
    description: "virtio entropy (RNG) driver",
    license: "GPL v2",
}
